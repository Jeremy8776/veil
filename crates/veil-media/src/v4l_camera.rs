//! Camera capture on Linux through Video4Linux2, with the `v4l` crate.
//!
//! Purpose: the same shape as the Windows `MfCamera`: open a camera by id in the mode that
//! suits a [`CameraMode`] best, then read packed NV12 pictures. Cameras send NV12, YUYV or
//! MJPEG; the last two are converted here ([`crate::yuv`], [`crate::mjpeg`]).
//!
//! Invariants:
//! - Modes are ranked by [`rank_modes`] and tried in order; a mode the driver changes when it
//!   is set (a different size or format comes back) is skipped, never used as if it were set.
//! - Every buffer from the driver is checked against the mode's size before it is read
//!   (`bytesused` and the converters' own checks), so a short buffer is an error.
//! - A camera that delivers nothing for [`STALL`] is an error, as on Windows.
//! - Timestamps are microseconds on this process's monotonic clock since the camera opened.
//! - No `unsafe` here; the `v4l` crate's own is listed in docs/DEPENDENCIES.md.

use std::io::ErrorKind;
use std::time::{Duration, Instant};

use v4l::buffer::Type;
use v4l::capability::Flags;
use v4l::frameinterval::FrameIntervalEnum;
use v4l::framesize::FrameSizeEnum;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, Format, FourCC, Fraction};

use crate::MediaError;
use crate::capture::{CameraMode, NativeMode, PixelFormat, rank_modes};
use crate::encoder::RawPicture;
use crate::mjpeg::mjpeg_to_nv12;
use crate::yuv::{Plane, planes_to_nv12, yuyv_to_nv12};

/// The longest the camera may go without a picture.
const STALL: Duration = Duration::from_secs(2);
/// Driver buffers in flight.
const BUFFERS: u32 = 4;

/// One camera, as Video4Linux lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub name: String,
    /// The device path, such as `/dev/video0`; pass it to [`V4lCamera::open`].
    pub id: String,
}

fn fourcc(format: PixelFormat) -> Option<FourCC> {
    match format {
        PixelFormat::Nv12 => Some(FourCC::new(b"NV12")),
        PixelFormat::Yuy2 => Some(FourCC::new(b"YUYV")),
        PixelFormat::Mjpeg => Some(FourCC::new(b"MJPG")),
        PixelFormat::Other => None,
    }
}

fn pixel_format(code: FourCC) -> PixelFormat {
    match &code.repr {
        b"NV12" => PixelFormat::Nv12,
        b"YUYV" => PixelFormat::Yuy2,
        b"MJPG" => PixelFormat::Mjpeg,
        _ => PixelFormat::Other,
    }
}

fn is_camera(device: &Device) -> bool {
    // UVC cameras also expose a metadata node per camera, which cannot capture video.
    let captures = device
        .query_caps()
        .is_ok_and(|caps| caps.capabilities.contains(Flags::VIDEO_CAPTURE));
    captures && device.enum_formats().is_ok_and(|formats| !formats.is_empty())
}

/// Every video capture device, in device number order.
pub fn cameras() -> Result<Vec<CameraInfo>, MediaError> {
    let mut nodes = v4l::context::enum_devices();
    nodes.sort_by_key(v4l::context::Node::index);
    Ok(nodes
        .into_iter()
        .filter_map(|node| {
            let id = node.path().to_string_lossy().into_owned();
            let device = Device::with_path(node.path()).ok()?;
            is_camera(&device).then(|| CameraInfo {
                name: node.name().unwrap_or_else(|| id.clone()),
                id,
            })
        })
        .collect())
}

/// Every mode the camera offers in a format Veil can convert.
fn native_modes(device: &Device) -> Vec<NativeMode> {
    let mut modes = Vec::new();
    let Ok(formats) = device.enum_formats() else {
        return modes;
    };
    for description in formats {
        let format = pixel_format(description.fourcc);
        if format == PixelFormat::Other {
            continue;
        }
        let Ok(sizes) = device.enum_framesizes(description.fourcc) else {
            continue;
        };
        for size in sizes {
            let (width, height) = match size.size {
                FrameSizeEnum::Discrete(d) => (d.width, d.height),
                FrameSizeEnum::Stepwise(s) => (s.max_width, s.max_height),
            };
            let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
                continue;
            };
            let Ok(intervals) = device.enum_frameintervals(description.fourcc, width, height) else {
                continue;
            };
            for interval in intervals {
                // The fastest rate a stepwise range allows, which is its smallest interval.
                let frac = match interval.interval {
                    FrameIntervalEnum::Discrete(f) => f,
                    FrameIntervalEnum::Stepwise(s) => s.min,
                };
                modes.push(NativeMode {
                    width: w,
                    height: h,
                    fps_num: frac.denominator,
                    fps_den: frac.numerator,
                    format,
                });
            }
        }
    }
    modes
}

/// Sets `mode` and returns true if the driver kept it exactly.
fn try_mode(device: &Device, mode: &NativeMode) -> bool {
    let Some(code) = fourcc(mode.format) else {
        return false;
    };
    let wanted = Format::new(u32::from(mode.width), u32::from(mode.height), code);
    let Ok(set) = device.set_format(&wanted) else {
        return false;
    };
    if (set.width, set.height, set.fourcc) != (wanted.width, wanted.height, wanted.fourcc) {
        return false;
    }
    // A driver that ignores the rate still captures; the encoder follows the real clock.
    let _ = device.set_params(&Parameters::new(Fraction::new(mode.fps_den, mode.fps_num)));
    true
}

/// An open camera giving NV12 pictures.
pub struct V4lCamera {
    stream: Stream<'static>,
    name: String,
    mode: NativeMode,
    /// Bytes per row of the driver's buffer, for NV12 and YUYV.
    stride: usize,
    started: Instant,
    scratch: Vec<u8>,
    picture: Vec<u8>,
    // Last, so the stream is released before the device closes.
    _device: Device,
}

impl V4lCamera {
    /// Opens the camera at this `id` (from [`cameras`]), or the first one for `None`, in the
    /// native mode that suits `want` best. The mode actually used is [`V4lCamera::mode`].
    pub fn open(id: Option<&str>, want: CameraMode) -> Result<Self, MediaError> {
        let info = match id {
            Some(id) => cameras()?
                .into_iter()
                .find(|c| c.id == id)
                .ok_or(MediaError::Unavailable("camera"))?,
            None => cameras()?
                .into_iter()
                .next()
                .ok_or(MediaError::Unavailable("camera"))?,
        };
        let device = Device::with_path(&info.id).map_err(|_| MediaError::Unavailable("camera"))?;
        let offered = native_modes(&device);
        let mode = rank_modes(&offered, want)
            .into_iter()
            .map(|i| offered[i])
            .find(|mode| try_mode(&device, mode))
            .ok_or(MediaError::Unavailable("camera mode"))?;
        let format = device.format().map_err(|_| MediaError::Capture("camera format"))?;
        let stride = usize::try_from(format.stride).map_err(|_| MediaError::Capture("camera format"))?;
        let mut stream = Stream::with_buffers(&device, Type::VideoCapture, BUFFERS).map_err(|e| {
            if e.kind() == ErrorKind::PermissionDenied {
                MediaError::Unavailable("camera permission")
            } else {
                MediaError::Capture("camera buffers")
            }
        })?;
        stream.set_timeout(STALL);
        Ok(Self {
            stream,
            name: info.name,
            mode,
            stride,
            started: Instant::now(),
            scratch: Vec::new(),
            picture: Vec::new(),
            _device: device,
        })
    }

    /// The camera's name, for the interface and logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The mode in use. Pictures come out at this size, in NV12; `format` is what the camera
    /// itself sends before conversion.
    pub fn mode(&self) -> NativeMode {
        self.mode
    }

    /// Waits for the next picture. `None` is a gap in the stream with no picture; call again.
    pub fn read(&mut self) -> Result<Option<RawPicture<'_>>, MediaError> {
        let (buffer, meta) = match self.stream.next() {
            Ok(next) => next,
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                return Err(MediaError::Capture("camera stalled"));
            }
            Err(_) => return Err(MediaError::Capture("camera read")),
        };
        let used = usize::try_from(meta.bytesused).unwrap_or(usize::MAX).min(buffer.len());
        let data = &buffer[..used];
        if data.is_empty() {
            return Ok(None);
        }
        let (w, h) = (self.mode.width, self.mode.height);
        let converted = match self.mode.format {
            PixelFormat::Mjpeg => mjpeg_to_nv12(data, w, h, &mut self.scratch, &mut self.picture),
            PixelFormat::Yuy2 => yuyv_to_nv12(data, self.stride, w, h, &mut self.picture),
            PixelFormat::Nv12 => {
                let split = self.stride * usize::from(h);
                let (luma, chroma) = data.split_at(split.min(data.len()));
                planes_to_nv12(
                    Plane { data: luma, pitch: self.stride },
                    Plane { data: chroma, pitch: self.stride },
                    w,
                    h,
                    &mut self.picture,
                )
            }
            PixelFormat::Other => Err(MediaError::Capture("camera format")),
        };
        // A damaged MJPEG picture happens on busy USB links; skip it rather than stop.
        if converted.is_err() && self.mode.format == PixelFormat::Mjpeg {
            return Ok(None);
        }
        converted?;
        let timestamp_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        RawPicture::new(w, h, usize::from(w), timestamp_us, &self.picture).map(Some)
    }
}
