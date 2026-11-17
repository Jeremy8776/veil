//! This operating system's media back end, under one set of names.
//!
//! Purpose: the app opens a [`Camera`], [`Microphone`], [`Speaker`] and video codec without
//! knowing which platform code runs. Each back end has the same methods (`open`, `mode`,
//! `read`, `write`), so only this module names them.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | Camera | Media Foundation | Video4Linux2 | AVFoundation |
//! | Microphone, speaker | WASAPI | ALSA (cpal) | Core Audio (cpal) |
//! | H.264 | Media Foundation | OpenH264 | OpenH264 |
//!
//! Invariants:
//! - Veil builds for Windows, Linux and macOS only; another target fails here, at compile
//!   time, rather than running without media.

use crate::ladder::Ladder;
use crate::{MediaError, VideoDecoder, VideoEncoder};

#[cfg(windows)]
pub use crate::ffi::windows::{
    AudioDevice, CameraInfo, MfCamera as Camera, WasapiCapture as Microphone,
    WasapiPlayback as Speaker, cameras, microphones, speakers,
};

#[cfg(not(windows))]
pub use crate::cpal_audio::{
    AudioDevice, CpalCapture as Microphone, CpalPlayback as Speaker, microphones, speakers,
};

#[cfg(target_os = "linux")]
pub use crate::v4l_camera::{CameraInfo, V4lCamera as Camera, cameras};

#[cfg(target_os = "macos")]
pub use crate::ffi::macos::{AvCamera as Camera, CameraInfo, cameras};

/// The H.264 simulcast encoder for `ladder`. On Windows a GPU encoder is tried first.
pub fn video_encoder(ladder: &Ladder) -> Result<Box<dyn VideoEncoder>, MediaError> {
    #[cfg(windows)]
    let encoder = crate::ffi::windows::media_foundation(ladder, true)?;
    #[cfg(not(windows))]
    let encoder = crate::ffi::openh264::openh264(ladder)?;
    Ok(Box::new(encoder))
}

/// An H.264 decoder for one sender's spatial layer.
pub fn video_decoder() -> Result<Box<dyn VideoDecoder>, MediaError> {
    #[cfg(windows)]
    let decoder = crate::ffi::windows::MfDecoder::open()?;
    #[cfg(not(windows))]
    let decoder = crate::ffi::openh264::OpenH264Decoder::open()?;
    Ok(Box::new(decoder))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("Veil's media back end is built for Windows, Linux and macOS only");
