//! Sharing your screen: the capture threads behind the Share button.
//!
//! Purpose: keeps the picker's list and thumbnails fresh, keeps a preview of the source whose
//! area is being chosen, and, once you press Share, hands about ten pictures a second to the
//! call engine's screen encoder (`engine/video_out.rs`). The camera carries on separately.
//!
//! Invariants:
//! - Nothing is captured for the call until you choose a source and press Share. Thumbnails and
//!   the picker's preview stay on this device.
//! - You can see what is sent: [`ScreenShare::take_sending`] gives the same converted pictures
//!   the encoder gets, cropped as they are sent.
//! - Losing focus changes nothing. A covered window is still captured, and a crop stays on the
//!   same part of its source. A minimised window has nothing to capture, so its last picture is
//!   repeated until it comes back.
//! - If the source cannot be captured for a second (a window closed, permission withdrawn),
//!   sharing stops and says so. It never moves to another source.
//! - Stop on the floating bar works on the capture thread directly, so it still stops sharing
//!   while Veil's main window is minimised.
//! - The tap never blocks capture: a full encoder queue drops the picture.
//! - Pictures keep flowing while the screen is still, so receivers can tell that sharing goes
//!   on: they drop a share that has been silent for a few seconds (`engine/video_in.rs`).
//! - Window titles are never logged or sent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use veil_media::screen::{self, Crop, Grabbed, ScreenCapture, ScreenChoice, ScreenSource};

use crate::convert::{Rgba, nv12_to_rgba};
use crate::engine::Captured;
use crate::media::{Worker, lock};

const SHARE_FPS: u32 = 10;
/// Your own view of the share is refreshed once every this many pictures.
const SENDING_EVERY: u32 = 3;
const PREVIEW_EVERY: Duration = Duration::from_secs(1);
const THUMBNAILS_EVERY: Duration = Duration::from_secs(2);
/// Preview pictures are shrunk to about this width.
const PREVIEW_WIDTH: u32 = 960;
const THUMBNAIL_WIDTH: u32 = 320;
/// Failed pictures in a row before sharing stops: one second's worth.
const MAX_FAILURES: u32 = SHARE_FPS;
const CANNOT_CAPTURE: &str = "Cannot capture this. Check that Veil may record the screen.";

#[derive(Default)]
struct State {
    tap: Mutex<Option<SyncSender<Captured>>>,
    choices: Mutex<Option<Vec<ScreenChoice>>>,
    thumbnails: Mutex<HashMap<ScreenSource, Rgba>>,
    preview: Mutex<Option<Rgba>>,
    /// The newest picture being sent, for your own view.
    sending: Mutex<Option<Rgba>>,
    error: Mutex<Option<String>>,
    /// Set by the capture thread when it ended by itself.
    stopped: AtomicBool,
    /// Stop was pressed on the floating bar.
    stop_asked: AtomicBool,
    bar_hidden: AtomicBool,
}

/// What the floating sharing bar can do. It runs apart from the main window.
#[derive(Clone)]
pub struct ShareBar(Arc<State>);

impl ShareBar {
    pub fn stop(&self) {
        self.0.stop_asked.store(true, Ordering::Release);
    }

    pub fn hide(&self) {
        self.0.bar_hidden.store(true, Ordering::Release);
    }
}

pub struct ScreenShare {
    ctx: egui::Context,
    state: Arc<State>,
    live: Option<Worker>,
    preview: Option<Worker>,
    browser: Option<Worker>,
}

impl ScreenShare {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            state: Arc::default(),
            live: None,
            preview: None,
            browser: None,
        }
    }

    /// Where shared pictures go, while in a call.
    pub fn set_tap(&self, tap: Option<SyncSender<Captured>>) {
        *lock(&self.state.tap) = tap;
    }

    pub fn is_sharing(&self) -> bool {
        self.live.is_some()
    }

    pub fn bar(&self) -> ShareBar {
        ShareBar(self.state.clone())
    }

    pub fn bar_hidden(&self) -> bool {
        self.state.bar_hidden.load(Ordering::Acquire)
    }

    /// Starts reading what can be shared, with thumbnails, until [`Self::close_picker`].
    pub fn browse(&mut self) {
        self.browser = None;
        *lock(&self.state.choices) = None;
        lock(&self.state.thumbnails).clear();
        *lock(&self.state.error) = None;
        let (state, ctx) = (self.state.clone(), self.ctx.clone());
        self.browser = Some(Worker::spawn(move |stop| browse_loop(&state, &ctx, &stop)));
    }

    /// What can be shared; `None` until the list has been read.
    pub fn choices(&self) -> Option<Vec<ScreenChoice>> {
        lock(&self.state.choices).clone()
    }

    /// Thumbnails made since the last call.
    pub fn take_thumbnails(&self) -> HashMap<ScreenSource, Rgba> {
        std::mem::take(&mut *lock(&self.state.thumbnails))
    }

    pub fn error(&self) -> Option<String> {
        lock(&self.state.error).clone()
    }

    /// Shows `source` for choosing an area, refreshed about once a second.
    pub fn start_preview(&mut self, source: ScreenSource) {
        self.stop_preview();
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

    pub fn close_picker(&mut self) {
        self.stop_preview();
        self.browser = None;
        lock(&self.state.thumbnails).clear();
    }

    /// Starts sharing `source`, or the part of it `crop` covers.
    pub fn start(&mut self, source: ScreenSource, crop: Option<Crop>) {
        self.close_picker();
        self.live = None;
        *lock(&self.state.error) = None;
        *lock(&self.state.sending) = None;
        self.state.stopped.store(false, Ordering::Release);
        self.state.stop_asked.store(false, Ordering::Release);
        self.state.bar_hidden.store(false, Ordering::Release);
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
        *lock(&self.state.sending) = None;
        self.close_picker();
    }

    /// The newest picture being sent, if one arrived since the last call.
    pub fn take_sending(&self) -> Option<Rgba> {
        lock(&self.state.sending).take()
    }

    /// True once after sharing ended by itself or from the floating bar.
    pub fn take_stopped(&mut self) -> bool {
        if !self.state.stopped.swap(false, Ordering::AcqRel) {
            return false;
        }
        self.live = None;
        *lock(&self.state.sending) = None;
        true
    }
}

fn give_up(state: &State, ctx: &egui::Context, why: &str) {
    crate::note!("screen share: gave up: {why}");
    *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
    state.stopped.store(true, Ordering::Release);
    ctx.request_repaint();
}

/// Sleeps for `time`, or until `stop` is set.
fn wait(time: Duration, stop: &AtomicBool) {
    let end = Instant::now() + time;
    while !stop.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn browse_loop(state: &State, ctx: &egui::Context, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match screen::open_all(std::process::id()) {
            Ok(sources) => {
                let list = sources.iter().map(|(choice, _)| choice.clone()).collect();
                *lock(&state.choices) = Some(list);
                ctx.request_repaint();
                for (choice, capture) in &sources {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Ok(Some(picture)) = capture.grab() {
                        let small = shrink(&picture, THUMBNAIL_WIDTH);
                        lock(&state.thumbnails).insert(choice.source, small);
                        ctx.request_repaint();
                    }
                }
            }
            Err(e) => {
                crate::note!("screen share: cannot list sources: {e}");
                *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
                lock(&state.choices).get_or_insert_with(Vec::new);
                ctx.request_repaint();
            }
        }
        wait(THUMBNAILS_EVERY, stop);
    }
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
    let mut size = None;
    let (mut failures, mut pictures) = (0, 0u32);
    while !stop.load(Ordering::Relaxed) {
        if state.stop_asked.swap(false, Ordering::AcqRel) {
            crate::note!("screen share: stopped from the bar");
            state.stopped.store(true, Ordering::Release);
            ctx.request_repaint();
            return;
        }
        let tick = Instant::now();
        match next_picture(&capture, crop, &mut nv12) {
            Next::Fresh(width, height) => {
                failures = 0;
                size = Some((width, height));
                if pictures.is_multiple_of(SENDING_EVERY) {
                    show_sending(state, ctx, &nv12, width, height);
                }
                pictures = pictures.wrapping_add(1);
            }
            // The last picture goes out again, so those watching know the share goes on.
            Next::Hold => failures = 0,
            Next::Failed => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return give_up(state, ctx, "the source stopped");
                }
            }
        }
        if let (Some((width, height)), Some(tap)) = (size, lock(&state.tap).as_ref()) {
            let _ = tap.try_send(Captured {
                width,
                height,
                stride: usize::from(width),
                fps: SHARE_FPS,
                timestamp_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                data: nv12.clone(),
            });
        }
        std::thread::sleep(period.saturating_sub(tick.elapsed()));
    }
}

fn show_sending(state: &State, ctx: &egui::Context, nv12: &[u8], width: u16, height: u16) {
    let (w, h) = (usize::from(width), usize::from(height));
    if let Some(rgba) = nv12_to_rgba(nv12, w, h, w, w > PREVIEW_WIDTH as usize) {
        *lock(&state.sending) = Some(rgba);
        ctx.request_repaint();
    }
}

enum Next {
    /// A new picture of this size is in the buffer.
    Fresh(u16, u16),
    /// The source cannot be seen for now: the buffer keeps the last picture.
    Hold,
    Failed,
}

fn next_picture(capture: &ScreenCapture, crop: Option<Crop>, nv12: &mut Vec<u8>) -> Next {
    let picture = match capture.grab() {
        Ok(Some(picture)) => picture,
        Ok(None) => return Next::Hold,
        Err(_) => return Next::Failed,
    };
    let Some(region) = screen::region(picture.width, picture.height, crop) else {
        return Next::Failed;
    };
    let size = (u16::try_from(region.width), u16::try_from(region.height));
    match (screen::to_nv12(&picture, region, nv12), size) {
        (Ok(()), (Ok(width), Ok(height))) => Next::Fresh(width, height),
        _ => Next::Failed,
    }
}

fn preview_loop(state: &State, ctx: &egui::Context, stop: &AtomicBool, source: ScreenSource) {
    let Ok(capture) = ScreenCapture::open(source) else {
        *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
        ctx.request_repaint();
        return;
    };
    while !stop.load(Ordering::Relaxed) {
        match capture.grab() {
            Ok(Some(picture)) => {
                *lock(&state.preview) = Some(shrink(&picture, PREVIEW_WIDTH));
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(_) => *lock(&state.error) = Some(CANNOT_CAPTURE.to_string()),
        }
        wait(PREVIEW_EVERY, stop);
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

    #[test]
    fn your_view_shows_the_picture_being_sent() {
        let state = State::default();
        // A 2x2 white NV12 picture.
        show_sending(&state, &egui::Context::default(), &[235, 235, 235, 235, 128, 128], 2, 2);
        let shown = lock(&state.sending).take().map(|p| (p.width, p.height, p.pixels[0]));
        assert_eq!(shown, Some((2, 2, 255)));
    }

    #[test]
    fn the_bar_asks_the_capture_thread_to_stop_and_can_be_hidden() {
        let share = ScreenShare::new(egui::Context::default());
        let bar = share.bar();
        bar.hide();
        assert!(share.bar_hidden());
        bar.stop();
        assert!(share.state.stop_asked.load(Ordering::Acquire));
    }
}
