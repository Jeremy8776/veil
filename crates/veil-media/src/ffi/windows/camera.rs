//! Windows camera capture through the Media Foundation source reader, to NV12.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: [`cameras`] lists the video capture devices, and [`MfCamera`] opens one in the
//! native mode [`rank_modes`] prefers and reads NV12 pictures from it. The source reader's
//! advanced video processing decodes MJPEG and converts YUY2 to NV12 where the camera cannot
//! give NV12 itself.
//!
//! Invariants:
//! - Every picture is copied out of the locked Media Foundation buffer before it is unlocked
//!   ([`crate::capture::pack_nv12`]), so a [`RawPicture`] never points into OS memory.
//! - The buffer's bounds come from Media Foundation (`Lock2DSize` or `Lock`), and the copy
//!   checks them, so a driver's odd pitch cannot read past the buffer.
//! - Field order in [`MfCamera`] matters: the reader is released, then the source is shut
//!   down, then Media Foundation and COM.
//!
//! shortcut: one CPU copy per picture (about 12 MB at 4K). Hand the GPU texture to the
//! encoder instead when capture moves to D3D11 (see `scale.rs`).

use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_SUBTYPE, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READERF_ERROR, MFCreateAttributes, MFCreateSourceReaderFromMediaSource,
    MFEnumDeviceSources, MFVideoFormat_MJPG, MFVideoFormat_NV12, MFVideoFormat_YUY2,
};
use windows::Win32::System::Com::CoTaskMemFree;

use super::enumerate::allocated_string;
use super::sample::{Nv12Layout, copy_nv12};
use super::{Runtime, media_type};
use crate::capture::{CameraMode, NativeMode, PixelFormat, rank_modes};
use crate::{MediaError, RawPicture};

/// The video stream, as the source reader numbers it.
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
/// More native types than any real camera offers; stops a broken driver looping forever.
const MAX_NATIVE_TYPES: u32 = 1_024;

/// One camera, as the operating system lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub name: String,
    /// Stable for this device on this machine; pass it to [`MfCamera::open`].
    pub id: String,
}

/// Every video capture device, in the order Windows lists them.
pub fn cameras() -> Result<Vec<CameraInfo>, MediaError> {
    let _runtime = Runtime::start()?;
    let devices = devices()?;
    Ok(devices.iter().map(info).collect())
}

/// An open camera giving NV12 pictures.
pub struct MfCamera {
    reader: IMFSourceReader,
    _source: SourceGuard,
    name: String,
    mode: NativeMode,
    /// Row pitch for a buffer that is not 2D; from the output type.
    stride: usize,
    picture: Vec<u8>,
    _runtime: Runtime,
}

/// Shuts the camera's media source down when dropped, after the reader is released.
struct SourceGuard(IMFMediaSource);

impl Drop for SourceGuard {
    fn drop(&mut self) {
        // SAFETY: the source is live; Shutdown takes no pointers and stops the camera.
        let _ = unsafe { self.0.Shutdown() };
    }
}

impl MfCamera {
    /// Opens the camera with this `id` (from [`cameras`]), or the first one for `None`, in the
    /// native mode that suits `want` best. The mode actually used is [`MfCamera::mode`].
    pub fn open(id: Option<&str>, want: CameraMode) -> Result<Self, MediaError> {
        let runtime = Runtime::start()?;
        let mut devices = devices()?;
        let index = match id {
            Some(id) => devices.iter().position(|d| info(d).id == id),
            None => (!devices.is_empty()).then_some(0),
        }
        .ok_or(MediaError::Unavailable("camera"))?;
        let device = devices.swap_remove(index);
        drop(devices);
        let name = info(&device).name;
        // SAFETY: `device` is a live activation object; this creates its media source.
        let source = unsafe { device.ActivateObject::<IMFMediaSource>() }
            .map_err(step("activate camera"))?;
        let source = SourceGuard(source);
        let (reader, mode, stride) = open_reader(&source.0, want)?;
        Ok(Self {
            reader,
            _source: source,
            name,
            mode,
            stride,
            picture: Vec::new(),
            _runtime: runtime,
        })
    }

    /// The camera's friendly name, for the interface and logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The mode in use. Pictures come out at this size and rate, in NV12; `format` is what
    /// the camera itself sends before conversion.
    pub fn mode(&self) -> NativeMode {
        self.mode
    }

    /// Waits for the next picture. `None` is a gap in the stream with no picture; call again.
    pub fn read(&mut self) -> Result<Option<RawPicture<'_>>, MediaError> {
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: the out values are valid for the call; the sample comes back owned.
        unsafe {
            self.reader.ReadSample(
                VIDEO,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        }
        .map_err(step("read picture"))?;
        if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err(MediaError::Capture("camera stream failed"));
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Err(MediaError::Capture("camera stopped"));
        }
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            let (mode, stride) = current_mode(&self.reader, self.mode.format)?;
            self.mode = mode;
            self.stride = stride;
        }
        let Some(sample) = sample else {
            return Ok(None);
        };
        self.copy_out(&sample)?;
        let width = self.mode.width;
        let timestamp_us = u64::try_from(timestamp / 10).unwrap_or(0);
        RawPicture::new(
            width,
            self.mode.height,
            usize::from(width),
            timestamp_us,
            &self.picture,
        )
        .map(Some)
    }

    /// Copies the sample's picture into `self.picture`, tightly packed.
    fn copy_out(&mut self, sample: &IMFSample) -> Result<(), MediaError> {
        let layout = Nv12Layout {
            width: self.mode.width,
            height: self.mode.height,
            stride: self.stride,
            alloc_rows: usize::from(self.mode.height),
        };
        copy_nv12(sample, layout, &mut self.picture, MediaError::Capture)
    }
}

/// Every video capture device's activation object.
fn devices() -> Result<Vec<IMFActivate>, MediaError> {
    let mut attributes: Option<IMFAttributes> = None;
    // SAFETY: `attributes` is a valid out value; the result is an owned interface.
    unsafe { MFCreateAttributes(&mut attributes, 1) }.map_err(step("attributes"))?;
    let attributes = attributes.ok_or(MediaError::Capture("attributes"))?;
    // SAFETY: `attributes` is live; both GUIDs are static.
    unsafe {
        attributes.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
    }
    .map_err(step("attributes"))?;
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: `attributes` is live, and `list` and `count` are valid out values.
    unsafe { MFEnumDeviceSources(&attributes, &mut list, &mut count) }
        .map_err(step("list cameras"))?;
    if list.is_null() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for index in 0..count as usize {
        // SAFETY: MFEnumDeviceSources returned an array of `count` owned activation pointers.
        // Each is moved out exactly once here, and the array is freed below without them.
        if let Some(device) = unsafe { list.add(index).read() } {
            found.push(device);
        }
    }
    // SAFETY: the array was allocated with CoTaskMemAlloc; its items were moved out above.
    unsafe { CoTaskMemFree(Some(list as *const _)) };
    Ok(found)
}

fn info(device: &IMFActivate) -> CameraInfo {
    CameraInfo {
        name: allocated_string(device, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
            .unwrap_or_else(|| String::from("camera")),
        id: allocated_string(
            device,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        )
        .unwrap_or_default(),
    }
}

/// Creates the reader and sets the best native mode that converts to NV12.
fn open_reader(
    source: &IMFMediaSource,
    want: CameraMode,
) -> Result<(IMFSourceReader, NativeMode, usize), MediaError> {
    let mut attributes: Option<IMFAttributes> = None;
    // SAFETY: `attributes` is a valid out value; the result is an owned interface.
    unsafe { MFCreateAttributes(&mut attributes, 1) }.map_err(step("attributes"))?;
    let attributes = attributes.ok_or(MediaError::Capture("attributes"))?;
    // SAFETY: `attributes` and `source` are live; the key is a static GUID.
    let reader = unsafe {
        attributes
            .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
            .map_err(step("attributes"))?;
        MFCreateSourceReaderFromMediaSource(source, &attributes).map_err(step("source reader"))?
    };
    // SAFETY: `reader` is live; these take stream numbers only.
    unsafe {
        reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
            .map_err(step("select stream"))?;
        reader
            .SetStreamSelection(VIDEO, true)
            .map_err(step("select stream"))?;
    }
    let natives = native_types(&reader);
    let modes: Vec<NativeMode> = natives.iter().map(|(_, mode)| *mode).collect();
    for index in rank_modes(&modes, want) {
        let (native, mode) = &natives[index];
        let size = (u64::from(mode.width) << 32) | u64::from(mode.height);
        let rate = (u64::from(mode.fps_num) << 32) | u64::from(mode.fps_den);
        let Ok(output) = media_type(MFVideoFormat_NV12, size, rate) else {
            continue;
        };
        // SAFETY: `reader` is live; `native` came from it and `output` is a live media type.
        let set = unsafe {
            reader
                .SetCurrentMediaType(VIDEO, None, native)
                .and_then(|()| reader.SetCurrentMediaType(VIDEO, None, &output))
        };
        if set.is_ok() {
            let (current, stride) = current_mode(&reader, mode.format)?;
            return Ok((reader, current, stride));
        }
    }
    Err(MediaError::Unavailable("camera mode that converts to NV12"))
}

/// The camera's native types, each with the mode it describes. Types without a readable size
/// or rate are skipped.
fn native_types(reader: &IMFSourceReader) -> Vec<(IMFMediaType, NativeMode)> {
    let mut found = Vec::new();
    for index in 0..MAX_NATIVE_TYPES {
        // SAFETY: `reader` is live; an index past the end returns an error.
        let Ok(native) = (unsafe { reader.GetNativeMediaType(VIDEO, index) }) else {
            break;
        };
        // SAFETY: `native` is live; the key is a static GUID.
        let format = match unsafe { native.GetGUID(&MF_MT_SUBTYPE) } {
            Ok(g) if g == MFVideoFormat_NV12 => PixelFormat::Nv12,
            Ok(g) if g == MFVideoFormat_YUY2 => PixelFormat::Yuy2,
            Ok(g) if g == MFVideoFormat_MJPG => PixelFormat::Mjpeg,
            _ => PixelFormat::Other,
        };
        if let Some(mode) = read_mode(&native, format) {
            found.push((native, mode));
        }
    }
    found
}

/// The reader's current NV12 output mode, and its row pitch.
fn current_mode(
    reader: &IMFSourceReader,
    format: PixelFormat,
) -> Result<(NativeMode, usize), MediaError> {
    // SAFETY: `reader` is live and the video stream is selected.
    let current = unsafe { reader.GetCurrentMediaType(VIDEO) }.map_err(step("current type"))?;
    let mode = read_mode(&current, format).ok_or(MediaError::Capture("current type"))?;
    // SAFETY: `current` is live; the key is a static GUID. A missing stride means packed rows.
    let stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .ok()
        .and_then(|s| usize::try_from(s as i32).ok())
        .filter(|&s| s >= usize::from(mode.width))
        .unwrap_or(usize::from(mode.width));
    Ok((mode, stride))
}

fn read_mode(media: &IMFMediaType, format: PixelFormat) -> Option<NativeMode> {
    // SAFETY: `media` is live; the keys are static GUIDs.
    let (size, rate) = unsafe {
        (
            media.GetUINT64(&MF_MT_FRAME_SIZE).ok()?,
            media.GetUINT64(&MF_MT_FRAME_RATE).ok()?,
        )
    };
    Some(NativeMode {
        width: u16::try_from(size >> 32).ok()?,
        height: u16::try_from(size & 0xFFFF_FFFF).ok()?,
        fps_num: (rate >> 32) as u32,
        fps_den: (rate & 0xFFFF_FFFF) as u32,
        format,
    })
}

fn step(name: &'static str) -> impl Fn(windows::core::Error) -> MediaError {
    move |_| MediaError::Capture(name)
}
