//! Sharing your screen: the capture threads behind the Share button.
//!
//! Purpose: lists what can be shared, keeps a slow preview of the source being chosen, and,
//! once you press Share, hands about ten pictures a second to the call engine's screen
//! encoder (`engine/video_out.rs`). The camera carries on separately.
//!
//! Invariants:
//! - Nothing is captured for the call until you choose a source and press Share. The picker's
//!   preview stays on this device.
//! - If the source cannot be captured for a second (a window closed, permission withdrawn),
//!   sharing stops and says so. It never moves to another source.
//! - The tap never blocks capture: a full encoder queue drops the picture.
//! - Pictures keep flowing while the screen is still, so receivers can tell that sharing goes
//!   on: they drop a share that has been silent for a few seconds (`engine/video_in.rs`).
//! - Window titles are never logged or sent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use veil_media::screen::{self, Crop, Grabbed, ScreenCapture, ScreenChoice, ScreenSource};

use crate::convert::Rgba;
use crate::engine::Captured;
use crate::media::{Worker, lock};

const SHARE_FPS: u32 = 10;
const PREVIEW_EVERY: Duration = Duration::from_secs(1);
/// Preview pictures are shrunk to about this width.
const PREVIEW_WIDTH: u32 = 960;
/// Failed pictures in a row before sharing stops: one second's worth.
const MAX_FAILURES: u32 = SHARE_FPS;
const CANNOT_CAPTURE: &str = "Cannot capture this. Check that Veil may record the screen.";

#[derive(Default)]
struct State {
    tap: Mutex<Option<SyncSender<Captured>>>,
    choices: Mutex<Option<Vec<ScreenChoice>>>,
    preview: Mutex<Option<Rgba>>,
    error: Mutex<Option<String>>,
    /// Set by the capture thread when it gave up.
    stopped: AtomicBool,
}

pub struct ScreenShare {
    ctx: egui::Context,
    state: Arc<State>,
    live: Option<Worker>,
    preview: Option<Worker>,
    lister: Option<Worker>,
}

impl ScreenShare {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            state: Arc::default(),
            live: None,
            preview: None,
            lister: None,
        }
    }

    /// Where shared pictures go, while in a call.
    pub fn set_tap(&self, tap: Option<SyncSender<Captured>>) {
        *lock(&self.state.tap) = tap;
    }

    pub fn is_sharing(&self) -> bool {
        self.live.is_some()
    }

    /// Reads what can be shared, on its own thread because listing windows can be slow.
    pub fn list(&mut self) {
        *lock(&self.state.choices) = None;
        *lock(&self.state.error) = None;
        let (state, ctx) = (self.state.clone(), self.ctx.clone());
        self.lister = Some(Worker::spawn(move |_stop| {
            let list = screen::choices(std::process::id()).unwrap_or_else(|e| {
                crate::note!("screen share: cannot list sources: {e}");
                *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
                Vec::new()
            });
            *lock(&state.choices) = Some(list);
            ctx.request_repaint();
        }));
    }

    /// What can be shared; `None` until the list has been read.
    pub fn choices(&self) -> Option<Vec<ScreenChoice>> {
        lock(&self.state.choices).clone()
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.state.error).clone()
    }

    /// Shows `source` in the picker, refreshed about once a second.
    pub fn start_preview(&mut self, source: ScreenSource) {
        self.preview = None;
        *lock(&self.state.preview) = None;
        *lock(&self.state.error) = None;
        let (state, ctx) = (self.state.clone(), self.ctx.clone());
        self.preview = Some(Worker::spawn(move |stop| {
            preview_loop(&state, &ctx, &stop, source);
        }));
    }

    pub fn stop_preview(&mut self) {
        self.preview = None;
        *lock(&self.state.preview) = None;
    }

    pub fn take_preview(&self) -> Option<Rgba> {
        lock(&self.state.preview).take()
    }

    /// Starts sharing `source`, or the part of it `crop` covers.
    pub fn start(&mut self, source: ScreenSource, crop: Option<Crop>) {
        self.stop_preview();
        self.live = None;
        *lock(&self.state.error) = None;
        self.state.stopped.store(false, Ordering::Release);
        let (state, ctx) = (self.state.clone(), self.ctx.clone());
        crate::note!("screen share: started");
        self.live = Some(Worker::spawn(move |stop| {
            share_loop(&state, &ctx, &stop, source, crop);
        }));
    }

    pub fn stop(&mut self) {
        if self.live.take().is_some() {
            crate::note!("screen share: stopped");
        }
        self.stop_preview();
    }

    /// True once after sharing stopped by itself.
    pub fn take_stopped(&mut self) -> bool {
        if !self.state.stopped.swap(false, Ordering::AcqRel) {
            return false;
        }
        self.live = None;
        true
    }
}

fn give_up(state: &State, ctx: &egui::Context, why: &str) {
    crate::note!("screen share: gave up: {why}");
    *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
    state.stopped.store(true, Ordering::Release);
    ctx.request_repaint();
}

fn share_loop(
    state: &State,
    ctx: &egui::Context,
    stop: &AtomicBool,
    source: ScreenSource,
    crop: Option<Crop>,
) {
    let capture = match ScreenCapture::open(source) {
        Ok(capture) => capture,
        Err(e) => return give_up(state, ctx, &e.to_string()),
    };
    let period = Duration::from_secs(1) / SHARE_FPS;
    let started = Instant::now();
    let mut nv12 = Vec::new();
    let mut failures = 0;
    while !stop.load(Ordering::Relaxed) {
        let tick = Instant::now();
        match next_picture(&capture, crop, &mut nv12) {
            Some((width, height)) => {
                failures = 0;
                if let Some(tap) = lock(&state.tap).as_ref() {
                    let _ = tap.try_send(Captured {
                        width,
                        height,
                        stride: usize::from(width),
                        fps: SHARE_FPS,
                        timestamp_us: u64::try_from(started.elapsed().as_micros())
                            .unwrap_or(u64::MAX),
                        data: nv12.clone(),
                    });
                }
            }
            None => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return give_up(state, ctx, "the source stopped");
                }
            }
        }
        std::thread::sleep(period.saturating_sub(tick.elapsed()));
    }
}

/// Grabs and converts one picture. Returns its size.
fn next_picture(
    capture: &ScreenCapture,
    crop: Option<Crop>,
    nv12: &mut Vec<u8>,
) -> Option<(u16, u16)> {
    let picture = capture.grab().ok()?;
    let region = screen::region(picture.width, picture.height, crop)?;
    screen::to_nv12(&picture, region, nv12).ok()?;
    Some((
        u16::try_from(region.width).ok()?,
        u16::try_from(region.height).ok()?,
    ))
}

fn preview_loop(state: &State, ctx: &egui::Context, stop: &AtomicBool, source: ScreenSource) {
    let Ok(capture) = ScreenCapture::open(source) else {
        *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
        ctx.request_repaint();
        return;
    };
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= next {
            next = Instant::now() + PREVIEW_EVERY;
            match capture.grab() {
                Ok(picture) => {
                    *lock(&state.preview) = Some(shrink(&picture, PREVIEW_WIDTH));
                    ctx.request_repaint();
                }
                Err(_) => *lock(&state.error) = Some(CANNOT_CAPTURE.to_string()),
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Keeps every n-th pixel, so the result is at most about `max_width` wide.
fn shrink(picture: &Grabbed, max_width: u32) -> Rgba {
    let step = (picture.width / max_width.max(1)).max(1) as usize;
    let (width, height) = (picture.width as usize, picture.height as usize);
    let (out_w, out_h) = (width.div_ceil(step), height.div_ceil(step));
    let mut pixels = Vec::with_capacity(out_w * out_h * 4);
    for y in (0..height).step_by(step) {
        for x in (0..width).step_by(step) {
            pixels.extend_from_slice(&picture.rgba[(y * width + x) * 4..][..4]);
        }
    }
    Rgba {
        width: out_w,
        height: out_h,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_picture_is_shrunk_and_a_small_one_kept() {
        let picture = Grabbed {
            width: 8,
            height: 4,
            rgba: vec![7; 8 * 4 * 4],
        };
        let small = shrink(&picture, 4);
        assert_eq!((small.width, small.height), (4, 2));
        assert_eq!(small.pixels.len(), 4 * 2 * 4);
        let same = shrink(&picture, 100);
        assert_eq!((same.width, same.height), (8, 4));
    }

    #[test]
    fn sharing_that_gave_up_reads_off_once_and_says_why() {
        let mut share = ScreenShare::new(egui::Context::default());
        share.live = Some(Worker::spawn(|_stop| {}));
        give_up(&share.state, &share.ctx, "test");
        assert!(share.take_stopped());
        assert!(!share.is_sharing());
        assert!(!share.take_stopped(), "reported once");
        assert_eq!(share.error().as_deref(), Some(CANNOT_CAPTURE));
    }
}
