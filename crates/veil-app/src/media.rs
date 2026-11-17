//! This device's camera and microphone, for the preview, the meter and the call.
//!
//! Purpose: each runs on its own thread, because the platform objects block on reads and are
//! not `Send`. The camera thread hands the newest picture to the interface; the microphone
//! thread hands over a level from 0 to 1. During a call, [`Taps`] also pass each picture and
//! each 20 ms of sound to the call engine, which encodes and encrypts them. A chosen
//! [`Blur`] and then [`Filter`] are applied to each picture before either of those, and the
//! microphone volume (`volume.rs`) to each 20 ms of sound. The speaker choice and volume live
//! here too ([`LocalMedia::playback`]) and the call's playback thread follows them.
//!
//! Invariants:
//! - Off means closed: turning the camera or microphone off stops its thread and releases the
//!   device, so the operating system's in-use light goes out too.
//! - Only the newest picture is kept; a slow interface skips pictures, never queues them.
//! - The preview and the taps see the same picture, after any blur and filter. With neither
//!   the picture is not copied or changed, and no blur worker thread exists.
//! - If the blur worker cannot keep up, the camera turns itself off (the picture that could
//!   not be blurred is neither shown nor sent) and a notice says why. Blur stays chosen, so the
//!   background is never sent sharp without the person deciding to.
//!   [`LocalMedia::take_camera_stopped`] tells the interface, which then shows the camera off.
//! - A device error is kept as text for the interface and the thread ends.
//! - The taps never block capture: a full engine queue drops the picture or frame.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use eframe::egui;

use crate::convert::Rgba;
use crate::devices::Choice;
use crate::engine::{Captured, Taps};
use crate::volume::{Gain, MIC_MAX, Playback, SPEAKER_MAX};
use veil_media::{Blur, BlurEngine, BlurStatus, Filter};

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
    filter: Mutex<Filter>,
    blur: Mutex<Blur>,
    /// Why the camera turned itself off for blur, until the camera or blur is chosen again.
    blur_notice: Mutex<Option<String>>,
    /// Set by the camera thread when it closed the camera itself.
    camera_stopped: AtomicBool,
    /// Microphone volume, applied before the meter and the call.
    mic_gain: Gain,
}

/// Shown when the camera turns itself off because blur cannot keep up.
pub const BLUR_TOO_BUSY: &str = "Camera off: this computer is too busy to blur your background.";

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

/// How long dropping a worker waits for its thread. Workers are dropped on the interface
/// thread, so a device driver stuck in a read must not hold it: Windows calls a window that
/// stops answering for 5 seconds "Not Responding" (logged as a cross-thread hang 16/11/2026).
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let Some(handle) = self.handle.take() else { return };
        let deadline = std::time::Instant::now() + STOP_WAIT;
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            // The thread still sees `stop` and ends when its device returns; it is left to
            // finish on its own rather than freeze the window.
            crate::note!("a device thread did not stop within {STOP_WAIT:?}; left to finish");
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
    /// The speaker and its volume, shared with the call's playback thread.
    playback: Arc<Playback>,
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
            playback: Arc::default(),
        }
    }

    /// For the call engine: the speaker to play through, which can change during the call.
    pub fn playback(&self) -> Arc<Playback> {
        self.playback.clone()
    }

    pub fn mic_volume(&self) -> f32 {
        self.shared.mic_gain.get()
    }

    /// Takes effect on the next 20 ms of sound, for the meter and the call alike.
    pub fn set_mic_volume(&self, volume: f32) {
        self.shared.mic_gain.set(volume, MIC_MAX);
    }

    pub fn speaker_volume(&self) -> f32 {
        self.playback.volume.get()
    }

    pub fn set_speaker_volume(&self, volume: f32) {
        self.playback.volume.set(volume, SPEAKER_MAX);
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
        *lock(&self.shared.blur_notice) = None;
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

    /// Chooses the speaker; the call's playback opens it (`engine/audio_out.rs`), at once
    /// during a call.
    pub fn choose_speaker(&mut self, id: Option<String>) {
        self.playback.set_device(id.clone());
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

    pub fn filter(&self) -> Filter {
        *lock(&self.shared.filter)
    }

    /// Takes effect on the next picture, for the preview and the call alike.
    pub fn set_filter(&self, filter: Filter) {
        *lock(&self.shared.filter) = filter;
    }

    pub fn blur(&self) -> Blur {
        *lock(&self.shared.blur)
    }

    /// Takes effect on the next picture. Choosing blur again clears an old notice.
    pub fn set_blur(&self, blur: Blur) {
        *lock(&self.shared.blur) = blur;
        if blur != Blur::Off {
            *lock(&self.shared.blur_notice) = None;
        }
    }

    /// True once after the camera thread closed the camera because blur could not keep up.
    /// Drops the ended worker, so [`Self::camera_on`] reads false from then on.
    pub fn take_camera_stopped(&mut self) -> bool {
        if !self.shared.camera_stopped.swap(false, Ordering::AcqRel) {
            return false;
        }
        self.camera = None;
        *lock(&self.shared.picture) = None;
        true
    }

    /// Why the camera turned itself off, if it did, until the camera or blur is chosen again.
    pub fn blur_notice(&self) -> Option<String> {
        lock(&self.shared.blur_notice).clone()
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

/// Which device a log line is about, without its name or id (`log.rs` keeps names out): the
/// one chosen in a picker, or the system default.
pub(crate) fn which(id: Option<&str>) -> &'static str {
    if id.is_some() {
        "the chosen device"
    } else {
        "the system default"
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

fn camera_loop(shared: &Shared, ctx: &egui::Context, stop: &AtomicBool, id: Option<&str>) {
    use veil_media::CameraMode;
    use veil_media::platform::Camera;
    let want = CameraMode {
        width: 1_280,
        height: 720,
        fps: 30,
    };
    let mut camera = match Camera::open(id, want) {
        Ok(camera) => {
            crate::note!("camera: opened {}", which(id));
            camera
        }
        Err(e) => {
            crate::note!("camera: cannot open {}: {e}", which(id));
            *lock(&shared.camera_error) = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    let mode = camera.mode();
    let fps = (mode.fps_num / mode.fps_den.max(1)).clamp(1, 60);
    // Changed copy of the picture; reused so effects cost no allocation per picture.
    let mut filtered = Vec::new();
    // Exists only while blur is on; dropping it stops the segmentation thread.
    let mut blur_engine: Option<BlurEngine> = None;
    while !stop.load(Ordering::Relaxed) {
        let picture = match camera.read() {
            Ok(Some(picture)) => picture,
            Ok(None) => continue,
            Err(e) => {
                *lock(&shared.camera_error) = Some(e.to_string());
                break;
            }
        };
        let (w, h) = (usize::from(picture.width), usize::from(picture.height));
        let filter = *lock(&shared.filter);
        let blur = *lock(&shared.blur);
        if blur == Blur::Off {
            blur_engine = None;
        }
        let pixels: &[u8] = if filter == Filter::None && blur == Blur::Off {
            picture.data
        } else {
            filtered.clear();
            filtered.extend_from_slice(picture.data);
            if blur != Blur::Off {
                let engine = blur_engine.get_or_insert_with(BlurEngine::start);
                if engine.apply(&mut filtered, w, h, picture.stride, blur) == BlurStatus::TooBusy {
                    // Never show or send this picture's background sharp: close the camera.
                    *lock(&shared.blur_notice) = Some(BLUR_TOO_BUSY.to_string());
                    shared.camera_stopped.store(true, Ordering::Release);
                    break;
                }
            }
            veil_media::effects::apply_filter(&mut filtered, w, h, picture.stride, filter);
            &filtered
        };
        if let Some(taps) = lock(&shared.taps).as_ref() {
            let _ = taps.video.try_send(Captured {
                width: picture.width,
                height: picture.height,
                stride: picture.stride,
                fps,
                timestamp_us: picture.timestamp_us,
                data: pixels.to_vec(),
            });
        }
        let half = w > 960;
        if let Some(rgba) = crate::convert::nv12_to_rgba(pixels, w, h, picture.stride, half) {
            *lock(&shared.picture) = Some(rgba);
            shared.picture_seq.fetch_add(1, Ordering::Release);
            ctx.request_repaint();
        }
    }
    ctx.request_repaint();
}

fn mic_loop(shared: &Shared, ctx: &egui::Context, stop: &AtomicBool, id: Option<&str>) {
    use veil_media::AudioSettings;
    use veil_media::platform::Microphone;
    let mut mic = match Microphone::open(&AudioSettings::voice(), id) {
        Ok(mic) => {
            crate::note!("microphone: opened {}", which(id));
            mic
        }
        Err(e) => {
            crate::note!("microphone: cannot open {}: {e}", which(id));
            *lock(&shared.mic_error) = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    let mut frames = 0u32;
    // The frame after the volume; reused so a volume costs no allocation per frame.
    let mut scaled = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match mic.read() {
            Ok(frame) => {
                scaled.clear();
                scaled.extend_from_slice(frame);
                crate::volume::scale(&mut scaled, shared.mic_gain.get());
                if let Some(taps) = lock(&shared.taps).as_ref() {
                    let _ = taps.audio.try_send(scaled.clone());
                }
                shared
                    .level
                    .store(meter(&scaled).to_bits(), Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::meter;

    #[test]
    fn a_stuck_worker_does_not_hold_whoever_drops_it() {
        use std::time::{Duration, Instant};
        // A driver stuck in a read: the thread ignores `stop` for 3 seconds.
        let stuck = super::Worker::spawn(|_stop| std::thread::sleep(Duration::from_secs(3)));
        let t = Instant::now();
        drop(stuck);
        assert!(t.elapsed() < Duration::from_millis(1_500), "held for {:?}", t.elapsed());

        let quick = super::Worker::spawn(|stop| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let t = Instant::now();
        drop(quick);
        assert!(t.elapsed() < Duration::from_millis(200), "a worker that stops is joined at once");
    }

    #[test]
    fn a_camera_stopped_for_blur_reads_off_once_and_keeps_blur_chosen() {
        use std::sync::atomic::Ordering;
        let mut media = super::LocalMedia::new(eframe::egui::Context::default());
        media.set_blur(veil_media::Blur::Strong);
        // A finished worker stands in for the camera thread that closed the camera itself.
        media.camera = Some(super::Worker::spawn(|_stop| {}));
        *super::lock(&media.shared.blur_notice) = Some(super::BLUR_TOO_BUSY.to_string());
        media.shared.camera_stopped.store(true, Ordering::Release);
        assert!(media.take_camera_stopped());
        assert!(!media.camera_on());
        assert!(!media.take_camera_stopped(), "reported once");
        assert_eq!(media.blur(), veil_media::Blur::Strong, "blur stays chosen");
        assert_eq!(media.blur_notice().as_deref(), Some(super::BLUR_TOO_BUSY));
    }

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
