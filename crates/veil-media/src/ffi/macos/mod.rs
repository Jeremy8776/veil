//! macOS: camera capture through AVFoundation (`camera.rs`) and the screen capture checks
//! `xcap` lacks (`screen.rs`), and computer sound for a screen share through ScreenCaptureKit
//! (`system_sound.rs`). Microphone and speakers go through cpal
//! ([`crate::cpal_audio`]) and H.264 through OpenH264 ([`super::openh264`]).
//!
//! Invariants:
//! - This is the only `unsafe` code on macOS apart from OpenH264. Every block says why it
//!   is sound.
//! - Objective-C objects are held as `Retained`, so each is released exactly once.

#![allow(unsafe_code)]

mod camera;
mod screen;
mod system_sound;

pub use camera::{AvCamera, CameraInfo, cameras};
pub use screen::{may_capture_screen, window_exists};
pub use system_sound::{SckAudio, sck_audio_support};
