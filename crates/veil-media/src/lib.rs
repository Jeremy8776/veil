//! Veil media pipeline: what to encode, how encoded pictures are framed, and when a receiver
//! can decode.
//!
//! Purpose: the parts of capture and encoding that do not depend on the operating system.
//! [`Schedule`] decides, for every captured picture, which spatial and temporal layers to
//! encode and whether it is a keyframe. [`VideoHeader`] goes in front of every encoded layer,
//! inside the sealed frame. [`Gate`] tells a receiver whether a layer can be decoded, and when
//! to ask the sender for a keyframe. Per-OS encoders implement [`VideoEncoder`] and decoders
//! [`VideoDecoder`]. Audio is
//! Opus ([`OpusEncoder`], [`OpusDecoder`]) with an [`AudioHeader`] in front of each packet,
//! put back in order by a [`JitterBuffer`]. A [`Denoiser`] removes background noise from speech and [`AutoGain`] evens out its level.
//!
//! Invariants:
//! - `unsafe` only in [`ffi`], one module per OS, under the rules in `docs/TRANSPARENCY.md`
//!   section 5. Everything else here is safe Rust and denies `unsafe_code`.
//! - Nothing here touches the network or keys. The app seals each payload with
//!   `FrameSender::encrypt_layer`, using the [`veil_security::Layer`] the schedule chose.
//! - Time is passed in, so keyframe timing is tested without sleeping.

pub mod annexb;
pub mod attention;
pub mod audio;
pub mod autogain;
pub mod blur;
pub mod capture;
// Also built for tests on Windows, where it compiles against WASAPI.
#[cfg(any(not(windows), test))]
pub mod cpal_audio;
pub mod decoder;
pub mod denoise;
pub mod effects;
pub mod encoder;
pub mod error;
pub mod ffi;
#[cfg(target_os = "linux")]
mod freedesktop;
pub mod gate;
pub mod jitter;
pub mod ladder;
#[cfg(any(target_os = "linux", test))]
pub mod mjpeg;
#[cfg(target_os = "macos")]
mod notify_macos;
pub mod payload;
pub mod platform;
pub mod remix;
pub mod scale;
pub mod schedule;
#[cfg(not(target_os = "android"))]
pub mod screen;
#[cfg(target_os = "android")]
#[path = "screen/mobile.rs"]
pub mod screen;
pub mod segment;
pub mod simulcast;
#[cfg(not(target_os = "android"))]
pub mod system_sound;
pub mod temporal;
#[cfg(target_os = "linux")]
pub mod v4l_camera;
pub mod yuv;

pub use audio::{AudioHeader, AudioSettings, Channels};
pub use autogain::AutoGain;
pub use blur::{Blur, BlurEngine, BlurStatus};
pub use capture::{CameraMode, NativeMode, PixelFormat};
pub use decoder::VideoDecoder;
pub use denoise::Denoiser;
pub use effects::Filter;
pub use encoder::{EncodedLayer, RawPicture, VideoEncoder};
pub use error::MediaError;
pub use ffi::opus::{OpusDecoder, OpusEncoder, opus_version};
pub use gate::{Gate, Verdict};
pub use jitter::{JitterBuffer, Playout};
pub use ladder::{Ladder, Rung, Tier};
pub use payload::{Codec, VideoHeader};
pub use schedule::{PicturePlan, Schedule};
pub use simulcast::{RungEncoder, Simulcast};

#[cfg(windows)]
pub use ffi::windows::{
    AudioDevice, CameraInfo, MediaFoundationEncoder, MfCamera, MfDecoder, MfRung, WasapiCapture,
    WasapiPlayback, cameras, media_foundation, microphones, protect, speakers, unprotect,
};
