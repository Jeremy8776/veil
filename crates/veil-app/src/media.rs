//! This device's camera and microphone, for the preview, the meter and the call.
//!
//! Purpose: each runs on its own thread, because the platform objects block on reads and are
//! not `Send`. The camera thread hands the newest picture to the interface; the microphone
//! thread hands over a level from 0 to 1. During a call, [`Taps`] also pass each picture and
//! each 20 ms of sound to the call engine, which encodes and encrypts them.
//!
//! Invariants:
//! - Off means closed: turning the camera or microphone off stops its thread and releases the
//!   device, so the operating system's in-use light goes out too.
//! - Only the newest picture is kept; a slow interface skips pictures, never queues them.
//! - A device error is kept as text for the interface and the thread ends.
//! - The taps never block capture: a full engine queue drops the picture or frame.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use eframe::egui;

use crate::convert::Rgba;
use crate::devices::Choice;
use crate::engine::{Captured, Taps};

/// What the interface reads from the workers.
#[derive(Default)]
struct Shared {
    picture: Mutex<Option<Rgba>>,
    picture_seq: AtomicU64,
    /// Microphone level, 0 to 1, as `f32` bits.
    level: AtomicU32,
    camera_error: Mutex<Option<String>>,
    mic_error: Mutex<Option<String>>,
    /// Where the call engine takes pictures and sound, while in a call.
    taps: Mutex<Option<Taps>>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A worker that panicked mid-write leaves plain data behind; use it.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A named thread with a stop flag. Dropping it asks the thread to stop and waits for it.
pub(crate) struct Worker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn spawn(body: impl FnOnce(Arc<AtomicBool>) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("veil-media".into())
            .spawn(move || body(flag))
            .ok();
        Self { stop, handle }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct LocalMedia {
    ctx: egui::Context,
    shared: Arc<Shared>,
    camera: Option<Worker>,
    mic: Option<Worker>,
    seen_seq: u64,
    /// Which devices to open; `None` is the default.
    choice: Choice,
}

impl LocalMedia {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            shared: Arc::default(),
            camera: None,
            mic: None,
            seen_seq: 0,
            choice: Choice::default(),
        }
    }

    pub fn camera_on(&self) -> bool {
        self.camera.is_some()
    }

    pub fn mic_on(&self) -> bool {
        self.mic.is_some()
    }

    pub fn set_camera(&mut self, on: bool) {
        if on == self.camera_on() {
            return;
        }
        if !on {
            self.camera = None;
            *lock(&self.shared.picture) = None;
            return;
        }
        *lock(&self.shared.camera_error) = None;
        let (shared, ctx) = (self.shared.clone(), self.ctx.clone());
        let id = self.choice.camera.clone();
        self.camera = Some(Worker::spawn(move |stop| {
            camera_loop(&shared, &ctx, &stop, id.as_deref());
        }));
    }

    pub fn set_mic(&mut self, on: bool) {
        if on == self.mic_on() {
            return;
        }
        if !on {
            self.mic = None;
            self.shared.level.store(0, Ordering::Relaxed);
            return;
        }
        *lock(&self.shared.mic_error) = None;
        let (shared, ctx) = (self.shared.clone(), self.ctx.clone());
        let id = self.choice.mic.clone();
        self.mic = Some(Worker::spawn(move |stop| {
            mic_loop(&shared, &ctx, &stop, id.as_deref());
        }));
    }

    pub fn choice(&self) -> &Choice {
        &self.choice
    }

    /// Switches to another camera; an open camera is closed and the new one opened.
    pub fn choose_camera(&mut self, id: Option<String>) {
        if self.choice.camera == id {
            return;
        }
        self.choice.camera = id;
        if self.camera_on() {
            self.set_camera(false);
            self.set_camera(true);
        }
    }

    /// Switches to another microphone, reopening it if it is on.
    pub fn choose_mic(&mut self, id: Option<String>) {
        if self.choice.mic == id {
            return;
        }
        self.choice.mic = id;
        if self.mic_on() {
            self.set_mic(false);
            self.set_mic(true);
        }
    }

    /// Remembers the speaker; the call's playback opens it (`engine/audio_out.rs`).
    pub fn choose_speaker(&mut self, id: Option<String>) {
        self.choice.speaker = id;
    }

    /// The newest camera picture, if one arrived since the last call.
    pub fn take_picture(&mut self) -> Option<Rgba> {
        let seq = self.shared.picture_seq.load(Ordering::Acquire);
        if seq == self.seen_seq {
            return None;
        }
        self.seen_seq = seq;
        lock(&self.shared.picture).take()
    }

    /// Starts or stops handing pictures and sound to the call engine.
    pub fn set_taps(&self, taps: Option<Taps>) {
        *lock(&self.shared.taps) = taps;
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    pub fn camera_error(&self) -> Option<String> {
        lock(&self.shared.camera_error).clone()
    }

    pub fn mic_error(&self) -> Option<String> {
        lock(&self.shared.mic_error).clone()
    }
}

/// Maps an RMS level to 0..1 on a 60 dB scale.
pub fn meter(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(windows)]
fn camera_loop(shared: &Shared, ctx: &egui::Context, stop: &AtomicBool, id: Option<&str>) {
    use veil_media::{CameraMode, MfCamera};
    let want = CameraMode {
        width: 1_280,
        height: 720,
        fps: 30,
    };
    let mut camera = match MfCamera::open(id, want) {
        Ok(camera) => camera,
        Err(e) => {
            *lock(&shared.camera_error) = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    let mode = camera.mode();
    let fps = (mode.fps_num / mode.fps_den.max(1)).clamp(1, 60);
    while !stop.load(Ordering::Relaxed) {
        let picture = match camera.read() {
            Ok(Some(picture)) => picture,
            Ok(None) => continue,
            Err(e) => {
                *lock(&shared.camera_error) = Some(e.to_string());
                break;
            }
        };
        if let Some(taps) = lock(&shared.taps).as_ref() {
            let _ = taps.video.try_send(Captured {
                width: picture.width,
                height: picture.height,
                stride: picture.stride,
                fps,
                timestamp_us: picture.timestamp_us,
                data: picture.data.to_vec(),
            });
        }
        let (w, h) = (usize::from(picture.width), usize::from(picture.height));
        let half = w > 960;
        if let Some(rgba) = crate::convert::nv12_to_rgba(picture.data, w, h, picture.stride, half) {
            *lock(&shared.picture) = Some(rgba);
            shared.picture_seq.fetch_add(1, Ordering::Release);
            ctx.request_repaint();
        }
    }
    ctx.request_repaint();
}

#[cfg(windows)]
fn mic_loop(shared: &Shared, ctx: &egui::Context, stop: &AtomicBool, id: Option<&str>) {
    use veil_media::{AudioSettings, WasapiCapture};
    let mut mic = match WasapiCapture::open(&AudioSettings::voice(), id) {
        Ok(mic) => mic,
        Err(e) => {
            *lock(&shared.mic_error) = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    let mut frames = 0u32;
    while !stop.load(Ordering::Relaxed) {
        match mic.read() {
            Ok(frame) => {
                if let Some(taps) = lock(&shared.taps).as_ref() {
                    let _ = taps.audio.try_send(frame.to_vec());
                }
                shared
                    .level
                    .store(meter(frame).to_bits(), Ordering::Relaxed);
                // Every other 20 ms frame is smooth enough for a meter.
                frames = frames.wrapping_add(1);
                if frames.is_multiple_of(2) {
                    ctx.request_repaint();
                }
            }
            Err(e) => {
                *lock(&shared.mic_error) = Some(e.to_string());
                break;
            }
        }
    }
    shared.level.store(0, Ordering::Relaxed);
}

#[cfg(not(windows))]
fn camera_loop(shared: &Shared, _ctx: &egui::Context, _stop: &AtomicBool, _id: Option<&str>) {
    *lock(&shared.camera_error) = Some("camera capture is Windows only so far".into());
}

#[cfg(not(windows))]
fn mic_loop(shared: &Shared, _ctx: &egui::Context, _stop: &AtomicBool, _id: Option<&str>) {
    *lock(&shared.mic_error) = Some("microphone capture is Windows only so far".into());
}

#[cfg(test)]
mod tests {
    use super::meter;

    #[test]
    fn meter_spans_sixty_decibels() {
        assert_eq!(meter(&[]), 0.0);
        assert_eq!(meter(&[0.0; 960]), 0.0);
        assert!((meter(&[1.0; 960]) - 1.0).abs() < 1e-6);
        // -30 dBFS is half way.
        let half = 10f32.powf(-30.0 / 20.0);
        assert!((meter(&[half; 960]) - 0.5).abs() < 1e-3);
    }
}
