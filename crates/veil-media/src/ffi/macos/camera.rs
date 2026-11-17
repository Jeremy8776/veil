//! Camera capture on macOS through AVFoundation.
//!
//! Purpose: the same shape as the Windows `MfCamera`: open a camera by id in the mode that
//! suits a [`CameraMode`] best, then read packed NV12 pictures. AVFoundation converts every
//! camera to NV12 ("420v") for us; a delegate copies each picture out on AVFoundation's
//! queue and hands it over through a bounded channel.
//!
//! Invariants:
//! - Camera permission is asked for once (macOS shows its own prompt); a refusal is
//!   [`MediaError::Unavailable`], never a stream of black pictures.
//! - The delegate holds a pixel buffer locked only while it copies the picture out, and
//!   reads only within the plane sizes CoreVideo reports.
//! - The delegate never blocks AVFoundation's queue: a full channel drops the picture.
//! - A camera that delivers nothing for [`STALL`] is an error, as on Windows.
//! - Timestamps are microseconds on this process's monotonic clock since the camera opened.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceFormat,
    AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession, AVCaptureVideoDataOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use crate::MediaError;
use crate::capture::{CameraMode, NativeMode, PixelFormat, rank_modes};
use crate::encoder::RawPicture;
use crate::yuv::{Plane, planes_to_nv12};

/// The longest the camera may go without a picture.
const STALL: Duration = Duration::from_secs(2);
/// How long to wait for the person to answer macOS's camera prompt.
const PERMISSION_WAIT: Duration = Duration::from_secs(120);
/// Pictures held for a slow reader.
const QUEUE: usize = 2;

/// One camera, as AVFoundation lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub name: String,
    /// AVFoundation's unique id; pass it to [`AvCamera::open`].
    pub id: String,
}

fn video() -> Result<&'static NSString, MediaError> {
    // SAFETY: a static AVFoundation exports, read after the framework is loaded (linking it).
    unsafe { AVMediaTypeVideo }.ok_or(MediaError::Unavailable("AVFoundation"))
}

/// Every camera, in the order AVFoundation lists them.
pub fn cameras() -> Result<Vec<CameraInfo>, MediaError> {
    let media = video()?;
    // SAFETY: a class method taking a valid media type. Deprecated in favour of a discovery
    // session, which needs a device type list per macOS version; this one still lists all.
    #[allow(deprecated)]
    let devices = unsafe { AVCaptureDevice::devicesWithMediaType(media) };
    Ok(devices
        .iter()
        .map(|device| CameraInfo {
            // SAFETY: plain property reads on a live device.
            name: unsafe { device.localizedName() }.to_string(),
            // SAFETY: as above.
            id: unsafe { device.uniqueID() }.to_string(),
        })
        .collect())
}

/// Asks for camera permission if it has not been decided, and waits for the answer.
fn permission() -> Result<(), MediaError> {
    let media = video()?;
    // SAFETY: a class method taking a valid media type.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    if status != AVAuthorizationStatus::NotDetermined {
        return Err(MediaError::Unavailable("camera permission"));
    }
    let (tx, rx) = sync_channel(1);
    let block = RcBlock::new(move |granted: Bool| {
        let _ = tx.try_send(granted.as_bool());
    });
    // SAFETY: a valid media type and a block that only sends on a channel, from any queue.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block) };
    match rx.recv_timeout(PERMISSION_WAIT) {
        Ok(true) => Ok(()),
        _ => Err(MediaError::Unavailable("camera permission")),
    }
}

/// One picture copied out of AVFoundation.
struct Picture {
    width: u16,
    height: u16,
    nv12: Vec<u8>,
}

/// Copies the NV12 picture in `pixels`, which the caller has locked.
fn copy_locked(pixels: &CVPixelBuffer) -> Option<Picture> {
    let width = u16::try_from(CVPixelBufferGetWidth(pixels)).ok()?;
    let height = u16::try_from(CVPixelBufferGetHeight(pixels)).ok()?;
    let plane = |index: usize| -> Option<Plane<'_>> {
        let base = CVPixelBufferGetBaseAddressOfPlane(pixels, index).cast::<u8>().cast_const();
        if base.is_null() {
            return None;
        }
        let pitch = CVPixelBufferGetBytesPerRowOfPlane(pixels, index);
        let rows = CVPixelBufferGetHeightOfPlane(pixels, index);
        // SAFETY: the buffer is locked, and CoreVideo reports `rows` rows of `pitch` bytes at
        // `base` for this plane. The slice does not outlive the lock (the caller unlocks after
        // this function returns and the copy is made).
        let data = unsafe { std::slice::from_raw_parts(base, pitch.checked_mul(rows)?) };
        Some(Plane { data, pitch })
    };
    let (luma, chroma) = (plane(0)?, plane(1)?);
    let mut nv12 = Vec::new();
    planes_to_nv12(luma, chroma, width, height, &mut nv12).ok()?;
    Some(Picture { width, height, nv12 })
}

/// Where the delegate sends pictures.
struct Sink {
    pictures: SyncSender<Picture>,
}

impl Sink {
    fn take(&self, sample: &CMSampleBuffer) {
        // SAFETY: a live sample buffer handed to the delegate for the length of this call.
        let Some(pixels) = (unsafe { sample.image_buffer() }) else {
            return;
        };
        let flags = CVPixelBufferLockFlags::ReadOnly;
        // SAFETY: a live pixel buffer; balanced by the unlock below on every path.
        if unsafe { CVPixelBufferLockBaseAddress(&pixels, flags) } != 0 {
            return;
        }
        let picture = copy_locked(&pixels);
        // SAFETY: balances the successful lock above, with the same flags.
        unsafe { CVPixelBufferUnlockBaseAddress(&pixels, flags) };
        if let Some(picture) = picture {
            // A full channel means the reader is behind: drop this picture.
            let _ = self.pictures.try_send(picture);
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds no Drop impl.
    #[unsafe(super(NSObject))]
    #[name = "VeilCameraDelegate"]
    #[ivars = Sink]
    struct Delegate;

    // SAFETY: NSObject already conforms; nothing is added.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the method signature matches the protocol's.
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.ivars().take(sample);
        }
    }
);

impl Delegate {
    fn new(sink: Sink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(sink);
        // SAFETY: NSObject's designated initialiser, called once on a fresh allocation.
        unsafe { msg_send![super(this), init] }
    }
}

/// Every mode the camera offers, with the format and frame duration that give it.
fn native_modes(device: &AVCaptureDevice) -> Vec<(NativeMode, Retained<AVCaptureDeviceFormat>, CMTime)> {
    let mut modes = Vec::new();
    // SAFETY: plain property reads on a live device and its formats.
    for format in unsafe { device.formats() }.iter() {
        // SAFETY: as above.
        let description = unsafe { format.formatDescription() };
        // SAFETY: a video format's description, as `formats` returns only video formats here.
        let size = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
        let (Ok(width), Ok(height)) = (u16::try_from(size.width), u16::try_from(size.height)) else {
            continue;
        };
        // SAFETY: as above.
        for range in unsafe { format.videoSupportedFrameRateRanges() }.iter() {
            // SAFETY: as above.
            let rate = unsafe { range.maxFrameRate() };
            if !rate.is_finite() || rate <= 0.0 {
                continue;
            }
            let mode = NativeMode {
                width,
                height,
                fps_num: (rate * 1_000.0).round() as u32,
                fps_den: 1_000,
                format: PixelFormat::Nv12,
            };
            // SAFETY: as above.
            modes.push((mode, format.clone(), unsafe { range.minFrameDuration() }));
        }
    }
    modes
}

/// An open camera giving NV12 pictures.
pub struct AvCamera {
    session: Retained<AVCaptureSession>,
    pictures: Receiver<Picture>,
    name: String,
    mode: NativeMode,
    started: Instant,
    picture: Vec<u8>,
    _output: Retained<AVCaptureVideoDataOutput>,
    _delegate: Retained<Delegate>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

impl AvCamera {
    /// Opens the camera with this `id` (from [`cameras`]), or the default one for `None`, in
    /// the mode that suits `want` best. The mode actually used is [`AvCamera::mode`].
    pub fn open(id: Option<&str>, want: CameraMode) -> Result<Self, MediaError> {
        permission()?;
        let media = video()?;
        // SAFETY: class methods taking a valid id or media type.
        let device = unsafe {
            match id {
                Some(id) => AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id)),
                None => AVCaptureDevice::defaultDeviceWithMediaType(media),
            }
        }
        .ok_or(MediaError::Unavailable("camera"))?;
        let offered = native_modes(&device);
        let modes: Vec<NativeMode> = offered.iter().map(|(mode, _, _)| *mode).collect();
        let best = rank_modes(&modes, want)
            .first()
            .and_then(|&i| offered.get(i))
            .ok_or(MediaError::Unavailable("camera mode"))?;

        // SAFETY: a live device; the error case is returned, not used.
        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|_| MediaError::Unavailable("camera in use"))?;
        // SAFETY: plain object creation.
        let (session, output) = unsafe { (AVCaptureSession::new(), AVCaptureVideoDataOutput::new()) };

        // SAFETY: a static CoreVideo exports, read after the framework is loaded (linking it).
        let key = unsafe { kCVPixelBufferPixelFormatTypeKey };
        // SAFETY: CFString and NSString are toll-free bridged, so the static key is a valid
        // NSString for as long as the program runs.
        let key: &NSString = unsafe { &*std::ptr::from_ref(key).cast::<NSString>() };
        let value = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        let value: &AnyObject = &value;
        let settings = NSDictionary::from_slices(&[key], &[value]);

        let (tx, rx) = sync_channel(QUEUE);
        let delegate = Delegate::new(Sink { pictures: tx });
        let queue = DispatchQueue::new("veil.camera", None);
        // SAFETY: valid settings with an NSNumber pixel format, a delegate that conforms to the
        // protocol, and a serial queue, as the method requires.
        unsafe {
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&*queue),
            );
        }

        // SAFETY: session configuration on live objects, between begin and commit. The device
        // is locked only while its format is set, and the format and duration come from it.
        unsafe {
            session.beginConfiguration();
            if !session.canAddInput(&input) || !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(MediaError::Unavailable("camera session"));
            }
            session.addInput(&input);
            session.addOutput(&output);
            let (_, format, duration) = best;
            if device.lockForConfiguration().is_ok() {
                device.setActiveFormat(format);
                device.setActiveVideoMinFrameDuration(*duration);
                device.unlockForConfiguration();
            }
            session.commitConfiguration();
            session.startRunning();
        }

        Ok(Self {
            session,
            pictures: rx,
            // SAFETY: a plain property read on a live device.
            name: unsafe { device.localizedName() }.to_string(),
            mode: best.0,
            started: Instant::now(),
            picture: Vec::new(),
            _output: output,
            _delegate: delegate,
            _queue: queue,
        })
    }

    /// The camera's name, for the interface and logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The mode in use. Pictures come out in NV12.
    pub fn mode(&self) -> NativeMode {
        self.mode
    }

    /// Waits for the next picture. `None` is a gap in the stream with no picture; call again.
    pub fn read(&mut self) -> Result<Option<RawPicture<'_>>, MediaError> {
        let picture = match self.pictures.recv_timeout(STALL) {
            Ok(picture) => picture,
            Err(RecvTimeoutError::Timeout) => return Err(MediaError::Capture("camera stalled")),
            Err(RecvTimeoutError::Disconnected) => return Err(MediaError::Capture("camera lost")),
        };
        self.picture = picture.nv12;
        let timestamp_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let (w, h) = (picture.width, picture.height);
        RawPicture::new(w, h, usize::from(w), timestamp_us, &self.picture).map(Some)
    }
}

impl Drop for AvCamera {
    fn drop(&mut self) {
        // SAFETY: a live session; stopping it ends delegate calls before the delegate and
        // queue are released (they are dropped after this).
        unsafe { self.session.stopRunning() };
    }
}
