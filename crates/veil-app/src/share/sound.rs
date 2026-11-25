//! Computer sound for a screen share: the capture thread behind "Include computer sound".
//!
//! Purpose: while a share includes sound, reads what the computer plays (without Veil's own
//! call, `veil_media::system_sound`) 20 ms at a time, applies the sharer's level and mute, and
//! hands it to the call engine as a stream of its own. [`SoundShare::support`] tells the
//! picker whether this computer can.
//!
//! Invariants:
//! - Off unless chosen for this share. Nothing is captured before Share is pressed, and
//!   stopping the share, by any route, stops the capture: the thread also ends as soon as the
//!   screen capture has ended by itself or from the floating bar.
//! - Level and mute are read for every frame, so a change is heard within 20 ms of capture.
//!   Muted sends silence, so receivers can tell the share's sound is still there.
//! - If sound cannot be captured, the picture goes on and the reason is kept for the interface.
//! - The tap never blocks capture: a full engine queue drops the frame.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use tokio::sync::mpsc;

use super::State;
use crate::media::{Worker, lock};
use crate::volume::{Gain, SPEAKER_MAX};

const CANNOT_START: &str = "Computer sound could not start. Your screen is still shared.";
#[cfg_attr(target_os = "android", allow(dead_code))]
const STOPPED: &str = "Computer sound stopped. Your screen is still shared.";
/// Frames between repaints for the level meter: five a second.
#[cfg_attr(target_os = "android", allow(dead_code))]
const METER_EVERY: u32 = 10;

/// Whether this computer can share its sound.
#[cfg_attr(target_os = "android", allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundSupport {
    Ready,
    /// Why not, in a few words.
    Unavailable(&'static str),
}

/// What the interface and the capture thread share about the sound of a share.
#[derive(Default)]
pub struct SoundState {
    /// The sharer's level, 0 to [`SPEAKER_MAX`]. Starts at 100%.
    level: Gain,
    muted: AtomicBool,
    /// The share includes sound: set by Share, cleared by stopping.
    on: AtomicBool,
    /// The level of the last frame sent, as `f32` bits.
    meter: AtomicU32,
    tap: Mutex<Option<mpsc::Sender<Vec<f32>>>>,
    error: Mutex<Option<String>>,
}

impl SoundState {
    pub fn level(&self) -> f32 {
        self.level.get()
    }

    pub fn set_level(&self, level: f32) {
        self.level.set(level, SPEAKER_MAX);
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// True while the share includes computer sound.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    /// How loud the sound being sent is, 0 to 1.
    pub fn meter(&self) -> f32 {
        f32::from_bits(self.meter.load(Ordering::Relaxed))
    }

    /// The gain for the next frame: the level, or nothing while muted.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    fn gain(&self) -> f32 {
        if self.muted() { 0.0 } else { self.level() }
    }
}

pub struct SoundShare {
    ctx: egui::Context,
    state: Arc<SoundState>,
    live: Option<Worker>,
    /// What [`SoundShare::support`] found, once found.
    support: Arc<Mutex<Option<SoundSupport>>>,
    probing: bool,
}

impl SoundShare {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            state: Arc::default(),
            live: None,
            support: Arc::default(),
            probing: false,
        }
    }

    pub fn state(&self) -> Arc<SoundState> {
        self.state.clone()
    }

    /// Whether this computer can share its sound; `None` while that is being found out, off
    /// the interface thread.
    pub fn support(&mut self) -> Option<SoundSupport> {
        let found = lock(&self.support).clone();
        if found.is_none() && !self.probing {
            self.probing = true;
            let (support, ctx) = (self.support.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                *lock(&support) = Some(probe());
                ctx.request_repaint();
            });
        }
        found
    }

    /// Where the sound goes, while in a call.
    pub fn set_tap(&self, tap: Option<mpsc::Sender<Vec<f32>>>) {
        *lock(&self.state.tap) = tap;
    }

    /// Starts capturing for the share whose capture thread uses `share`.
    pub(super) fn start(&mut self, share: Arc<State>) {
        self.live = None;
        *lock(&self.state.error) = None;
        self.state.on.store(true, Ordering::Release);
        let (state, ctx) = (self.state.clone(), self.ctx.clone());
        self.live = Some(Worker::spawn(move |stop| {
            sound_loop(&state, &share, &ctx, &stop);
        }));
    }

    pub fn stop(&mut self) {
        if self.live.take().is_some() {
            crate::note!("share sound: stopped");
        }
        self.state.on.store(false, Ordering::Release);
        self.state.meter.store(0, Ordering::Relaxed);
    }

    /// Why sound stopped or could not start, while the share goes on. Given once.
    pub fn take_error(&self) -> Option<String> {
        lock(&self.state.error).take()
    }
}

fn probe() -> SoundSupport {
    #[cfg(not(target_os = "android"))]
    return match veil_media::system_sound::support() {
        veil_media::system_sound::Support::Ready => SoundSupport::Ready,
        veil_media::system_sound::Support::Unavailable(why) => SoundSupport::Unavailable(why),
    };
    #[cfg(target_os = "android")]
    SoundSupport::Unavailable("Not on this device yet")
}

/// Notes why sound could not start; the share goes on without it.
fn cannot_start(state: &SoundState, ctx: &egui::Context, why: &str) {
    crate::note!("share sound: cannot start: {why}");
    *lock(&state.error) = Some(CANNOT_START.to_string());
    state.on.store(false, Ordering::Release);
    ctx.request_repaint();
}

#[cfg(target_os = "android")]
fn sound_loop(state: &SoundState, _share: &State, ctx: &egui::Context, _stop: &AtomicBool) {
    cannot_start(state, ctx, "not on this device");
}

#[cfg(not(target_os = "android"))]
fn sound_loop(state: &SoundState, share: &State, ctx: &egui::Context, stop: &AtomicBool) {
    let mut sound = match veil_media::system_sound::SystemSound::open() {
        Ok(sound) => sound,
        Err(e) => return cannot_start(state, ctx, &e.to_string()),
    };
    crate::note!("share sound: started");
    let mut frame = Vec::new();
    let mut count = 0u32;
    while !stop.load(Ordering::Relaxed) && !share.stopped.load(Ordering::Acquire) {
        match sound.read() {
            Ok(pcm) => {
                frame.clear();
                frame.extend_from_slice(pcm);
                crate::volume::scale(&mut frame, state.gain());
                state
                    .meter
                    .store(crate::media::meter(&frame).to_bits(), Ordering::Relaxed);
                if let Some(tap) = lock(&state.tap).as_ref() {
                    let _ = tap.try_send(frame.clone());
                }
                count = count.wrapping_add(1);
                if count.is_multiple_of(METER_EVERY) {
                    ctx.request_repaint();
                }
            }
            Err(e) => {
                crate::note!("share sound: stopped by the system: {e}");
                *lock(&state.error) = Some(STOPPED.to_string());
                state.on.store(false, Ordering::Release);
                break;
            }
        }
    }
    state.meter.store(0, Ordering::Relaxed);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_and_mute_set_the_gain_and_stay_in_range() {
        let state = SoundState::default();
        assert_eq!(state.gain(), 1.0, "full level to start");
        state.set_level(0.3);
        assert_eq!(state.gain(), 0.3);
        state.set_muted(true);
        assert_eq!(state.gain(), 0.0, "muted sends silence");
        assert_eq!(state.level(), 0.3, "the level comes back on unmuting");
        state.set_level(4.0);
        assert_eq!(state.level(), SPEAKER_MAX);
    }

    #[test]
    fn stopping_turns_sound_off_and_clears_the_meter() {
        let mut sound = SoundShare::new(egui::Context::default());
        sound.state.on.store(true, Ordering::Release);
        sound.state.meter.store(0.5f32.to_bits(), Ordering::Relaxed);
        sound.stop();
        assert!(!sound.state().is_on());
        assert_eq!(sound.state().meter(), 0.0);
    }
}
