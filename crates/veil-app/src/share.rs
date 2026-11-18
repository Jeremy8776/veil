//! Sharing your screen: the capture threads behind the Share button.
//!
//! Purpose: keeps the picker's list and thumbnails fresh, keeps a preview of the source whose
//! area is being chosen, and, once you press Share, hands up to thirty pictures a second to the
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
//! - The picker never goes out in a share of the screen it is on. From the moment it is asked
//!   for until shortly after it closes, Veil's window is kept out of captures and the share
//!   stays live. Where that cannot be done, new pictures are thrown away instead and the last
//!   one sent before the picker opened is repeated.
//! - The tap never blocks capture: a full encoder queue drops the picture.
//! - Pictures keep flowing while the screen is still, so receivers can tell that sharing goes
//!   on: they drop a share that has been silent for a few seconds (`engine/video_in.rs`).
//! - Window titles are never logged or sent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use veil_media::screen::{self, Crop, ScreenCapture, ScreenChoice, ScreenSource};

use crate::convert::{Rgba, nv12_to_rgba};
use crate::engine::Captured;
use crate::media::{Worker, lock};

mod thumbnails;

use thumbnails::{browse_loop, shrink};

const SHARE_FPS: u32 = 30;
const PREVIEW_EVERY: Duration = Duration::from_millis(200);
/// Your own view of a picture wider than this is halved.
const PREVIEW_WIDTH: u32 = 960;
/// The largest size of the area preview, in pixels.
const PREVIEW_BOX: (u32, u32) = (1_280, 720);
/// Attempts to keep the floating bar out of captures: about five seconds' worth.
const MAX_BAR_TRIES: u32 = 50;
/// How long a failing source may show nothing before sharing stops.
const GIVE_UP_AFTER: Duration = Duration::from_secs(1);
const MIN_FAILURES: u32 = 3;
/// How long the picker gets to leave the screen after it closes.
const PICKER_CLEAR: Duration = Duration::from_millis(500);
const CANNOT_CAPTURE: &str = "Cannot capture this. Check that Veil may record the screen.";

/// How the picker is being kept out of the share.
#[derive(Clone, Copy, Debug, Default)]
enum Hold {
    #[default]
    Off,
    /// Veil's window is hidden from captures, so the share can stay live.
    Hidden,
    /// The picker has closed. The window comes back into captures at this time.
    HiddenUntil(Instant),
    /// The window could not be hidden, so outgoing pictures are frozen.
    Frozen,
    FrozenUntil(Instant),
}

impl Hold {
    fn freezes_pictures(self, now: Instant) -> bool {
        match self {
            Self::Off | Self::Hidden | Self::HiddenUntil(_) => false,
            Self::Frozen => true,
            Self::FrozenUntil(end) => now < end,
        }
    }

    /// What this becomes once the picker has closed at `now`.
    fn after_closing(self, now: Instant) -> Self {
        match self {
            Self::Hidden => Self::HiddenUntil(now + PICKER_CLEAR),
            Self::Frozen => Self::FrozenUntil(now + PICKER_CLEAR),
            other => other,
        }
    }
}

/// Keeps the picker that is about to open out of the share.
fn cover_picker(state: &State) {
    let mut hold = lock(&state.hold);
    *hold = if screen::set_main_window_hidden(crate::WINDOW_TITLE, true) {
        Hold::Hidden
    } else {
        Hold::Frozen
    };
}

#[derive(Default)]
struct State {
    hold: Mutex<Hold>,
    tap: Mutex<Option<SyncSender<Captured>>>,
    choices: Mutex<Option<Vec<ScreenChoice>>>,
    thumbnails: Mutex<HashMap<ScreenSource, Rgba>>,
    /// The picker is showing windows rather than screens.
    windows_tab: AtomicBool,
    preview: Mutex<Option<Rgba>>,
    /// The newest picture being sent, for your own view.
    sending: Mutex<Option<Rgba>>,
    error: Mutex<Option<String>>,
    /// Set by the capture thread when it ended by itself.
    stopped: AtomicBool,
    /// Stop was pressed on the floating bar.
    stop_asked: AtomicBool,
    /// Change was pressed on the floating bar.
    change_asked: AtomicBool,
    bar_hidden: AtomicBool,
    /// The bar no longer needs keeping out of captures.
    bar_settled: AtomicBool,
    bar_tries: AtomicU32,
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

    /// Asks for the picker. It is covered first, before Veil's window comes forward.
    pub fn change(&self) {
        cover_picker(&self.0);
        self.0.change_asked.store(true, Ordering::Release);
    }

    /// Keeps the bar's window, found by `title`, out of captures. True means try again soon:
    /// the window may not exist yet.
    pub fn keep_out_of_capture(&self, title: &str) -> bool {
        let state = &self.0;
        if state.bar_settled.load(Ordering::Acquire) {
            return false;
        }
        let tries = state.bar_tries.fetch_add(1, Ordering::AcqRel) + 1;
        let hidden = screen::hide_window_from_capture(title);
        if !hidden && tries >= MAX_BAR_TRIES {
            crate::note!("screen share: the bar could not be kept out of captures");
        }
        let settled = hidden || tries >= MAX_BAR_TRIES;
        state.bar_settled.store(settled, Ordering::Release);
        !settled
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

    /// True once after Change was pressed on the floating bar.
    pub fn take_change_asked(&self) -> bool {
        self.state.change_asked.swap(false, Ordering::AcqRel)
    }

    /// Starts reading what can be shared, with thumbnails, until [`Self::close_picker`].
    pub fn browse(&mut self) {
        cover_picker(&self.state);
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

    /// Says which of the picker's tabs is open, so only its thumbnails are kept fresh.
    pub fn set_windows_tab(&self, windows: bool) {
        self.state.windows_tab.store(windows, Ordering::Release);
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
        let mut hold = lock(&self.state.hold);
        *hold = hold.after_closing(Instant::now());
        drop(hold);
        self.ctx.request_repaint_after(PICKER_CLEAR);
    }

    /// Lets Veil's window back into captures once the closed picker has left the screen. Call
    /// this on every frame of the call screen.
    pub fn tick(&self) {
        let mut hold = lock(&self.state.hold);
        if let Hold::HiddenUntil(end) = *hold {
            let now = Instant::now();
            if now >= end {
                screen::set_main_window_hidden(crate::WINDOW_TITLE, false);
                *hold = Hold::Off;
            } else {
                self.ctx.request_repaint_after(end - now);
            }
        }
    }

    /// Lets Veil's window back into captures now, as nothing is being shared.
    fn uncover_now(&self) {
        let mut hold = lock(&self.state.hold);
        if matches!(*hold, Hold::Hidden | Hold::HiddenUntil(_)) {
            screen::set_main_window_hidden(crate::WINDOW_TITLE, false);
            *hold = Hold::Off;
        }
    }

    /// Starts sharing `source`, or the part of it `crop` covers.
    pub fn start(&mut self, source: ScreenSource, crop: Option<Crop>) {
        self.close_picker();
        self.live = None;
        *lock(&self.state.error) = None;
        *lock(&self.state.sending) = None;
        self.state.stopped.store(false, Ordering::Release);
        self.state.stop_asked.store(false, Ordering::Release);
        self.state.change_asked.store(false, Ordering::Release);
        self.state.bar_hidden.store(false, Ordering::Release);
        self.state.bar_settled.store(false, Ordering::Release);
        self.state.bar_tries.store(0, Ordering::Release);
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
        self.uncover_now();
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

/// True once a source has failed repeatedly and shown nothing for [`GIVE_UP_AFTER`]. Both are
/// needed: where one screenshot takes a long time, a single failure can outlast that time.
fn has_gone(failures: u32, since_last_picture: Duration) -> bool {
    failures >= MIN_FAILURES && since_last_picture >= GIVE_UP_AFTER
}

fn share_loop(
    state: &State,
    ctx: &egui::Context,
    stop: &AtomicBool,
    source: ScreenSource,
    crop: Option<Crop>,
) {
    let capture = match ScreenCapture::open_live(source) {
        Ok(capture) => capture,
        Err(e) => return give_up(state, ctx, &e.to_string()),
    };
    let period = Duration::from_secs(1) / SHARE_FPS;
    let started = Instant::now();
    let mut nv12 = Vec::new();
    let mut size = None;
    let mut failures = 0;
    let mut last_good = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if state.stop_asked.swap(false, Ordering::AcqRel) {
            crate::note!("screen share: stopped from the bar");
            state.stopped.store(true, Ordering::Release);
            ctx.request_repaint();
            return;
        }
        let tick = Instant::now();
        let held = lock(&state.hold).freezes_pictures(tick);
        let next = if held { discard_picture(&capture) } else { next_picture(&capture, crop, &mut nv12) };
        match next {
            Next::Fresh(width, height) => {
                failures = 0;
                last_good = Instant::now();
                size = Some((width, height));
                show_sending(state, ctx, &nv12, width, height);
            }
            // The last picture goes out again, so those watching know the share goes on.
            Next::Hold => {
                failures = 0;
                last_good = Instant::now();
            }
            Next::Failed => {
                failures += 1;
                if has_gone(failures, last_good.elapsed()) {
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
    /// Nothing new to see for now: the buffer keeps the last picture.
    Hold,
    Failed,
}

/// Reads and drops the newest picture, so none showing the picker stays queued. A source that
/// has gone is still noticed.
fn discard_picture(capture: &ScreenCapture) -> Next {
    match capture.grab() {
        Ok(_) => Next::Hold,
        Err(_) => Next::Failed,
    }
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
                *lock(&state.preview) = Some(shrink(&picture, PREVIEW_BOX));
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(_) => *lock(&state.error) = Some(CANNOT_CAPTURE.to_string()),
        }
        wait(PREVIEW_EVERY, stop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_has_gone_after_repeated_failures_and_a_second_without_pictures() {
        assert!(!has_gone(30, Duration::from_millis(900)), "too soon");
        assert!(!has_gone(1, Duration::from_secs(5)), "one slow failure");
        assert!(has_gone(MIN_FAILURES, GIVE_UP_AFTER));
    }

    #[test]
    fn pictures_freeze_only_when_the_window_could_not_be_hidden() {
        let now = Instant::now();
        assert!(!Hold::Off.freezes_pictures(now));
        assert!(!Hold::Hidden.freezes_pictures(now));
        assert!(!Hold::Hidden.after_closing(now).freezes_pictures(now));
        assert!(Hold::Frozen.freezes_pictures(now + Duration::from_secs(60)));
        let closing = Hold::Frozen.after_closing(now);
        assert!(closing.freezes_pictures(now));
        assert!(!closing.freezes_pictures(now + PICKER_CLEAR));
    }

    #[test]
    fn a_hidden_window_stays_hidden_until_the_picker_has_left() {
        let now = Instant::now();
        let closing = Hold::Hidden.after_closing(now);
        assert!(matches!(closing, Hold::HiddenUntil(end) if end == now + PICKER_CLEAR));
    }

    #[test]
    fn without_a_window_to_hide_change_freezes_until_the_picker_closes() {
        let mut share = ScreenShare::new(egui::Context::default());
        share.bar().change();
        assert!(matches!(*lock(&share.state.hold), Hold::Frozen));
        share.close_picker();
        assert!(matches!(*lock(&share.state.hold), Hold::FrozenUntil(_)));
    }

    #[test]
    fn change_on_the_bar_is_read_once() {
        let share = ScreenShare::new(egui::Context::default());
        share.bar().change();
        assert!(share.take_change_asked());
        assert!(!share.take_change_asked());
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
