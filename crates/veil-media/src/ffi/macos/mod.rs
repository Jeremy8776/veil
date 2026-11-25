//! macOS: camera capture through AVFoundation (`camera.rs`), the screen capture checks
//! `xcap` lacks (`screen.rs`) and the open panel for importing a LUT (`pick.rs`). Microphone
//! and speakers go through cpal ([`crate::cpal_audio`]) and H.264 through OpenH264
//! ([`super::openh264`]).
//!
//! Invariants:
//! - This is the only `unsafe` code on macOS apart from OpenH264. Every block says why it
//!   is sound.
//! - Objective-C objects are held as `Retained`, so each is released exactly once.

#![allow(unsafe_code)]

mod camera;
mod pick;
mod screen;

pub use camera::{AvCamera, CameraInfo, cameras};
pub use pick::pick_file;
pub use screen::{may_capture_screen, window_exists};
