//! Computer sound for a screen share: what the computer is playing, without Veil's own call.
//!
//! Purpose: [`SystemSound`] reads what this computer plays, one frame of
//! [`AudioSettings::share_sound`] at a time, for the share's own Opus stream. [`support`] says
//! whether this computer can, so the share picker can say so before anyone relies on it.
//!
//! | | Back end | Veil's own sound left out by |
//! |---|---|---|
//! | Windows | WASAPI process loopback (`ffi/windows/loopback.rs`) | excluding Veil's process tree |
//! | macOS | ScreenCaptureKit audio (`ffi/macos/system_sound.rs`) | `excludesCurrentProcessAudio` |
//! | Linux | PipeWire (`system_sound/linux.rs`) | linking every app's output except Veil's |
//!
//! Invariants:
//! - Veil's own playback (everyone else in the call) is never captured, so nobody hears
//!   themselves echoed back.
//! - Frames come at real time: a full frame as soon as the system has given one, or, once the
//!   frame is overdue, what has arrived padded with silence. Nothing playing is a stream of
//!   silent frames, not a stall, so the share's sound never stops looking alive.
//! - At most [`BACKLOG_FRAMES`] frames wait; older sound is dropped, so a slow reader does
//!   not fall behind for good.
//! - Capture stops when the [`SystemSound`] is dropped.

#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", target_os = "linux", test))]
use crate::MediaError;
use crate::audio::AudioSettings;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", test))]
pub mod graph;

#[cfg(windows)]
pub use crate::ffi::windows::LoopbackCapture as SystemSound;
#[cfg(target_os = "macos")]
pub use crate::ffi::macos::SckAudio as SystemSound;
#[cfg(target_os = "linux")]
pub use linux::PipeWireSound as SystemSound;

/// The most frames of computer sound held for a slow reader.
pub const BACKLOG_FRAMES: usize = 5;
/// How long after a frame is due the reader waits before padding it with silence.
const SLACK: Duration = Duration::from_millis(10);
/// A reader this far behind starts its clock again rather than catching up in a burst.
const RESYNC: Duration = Duration::from_millis(200);

/// Whether this computer can share its sound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Ready,
    /// Why not, in a few words for the share picker.
    Unavailable(&'static str),
}

/// Whether [`SystemSound::open`] can be expected to work here. May take a moment, so call it
/// off the interface thread.
pub fn support() -> Support {
    #[cfg(windows)]
    return crate::ffi::windows::loopback_support();
    #[cfg(target_os = "macos")]
    return crate::ffi::macos::sck_audio_support();
    #[cfg(target_os = "linux")]
    return linux::support();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    Support::Unavailable("Not available on this device")
}

/// Turns whatever the system delivers into whole frames at real time.
pub struct Assembler {
    /// Samples (all channels) in one frame.
    frame: usize,
    period: Duration,
    pending: Vec<f32>,
    out: Vec<f32>,
    /// When the next frame is due; set by the first [`Self::deadline`].
    next_due: Option<Instant>,
}

impl Assembler {
    pub fn new(settings: &AudioSettings) -> Self {
        let frame = settings.frame_samples() * settings.channels.count();
        Self {
            frame,
            period: Duration::from_millis(u64::from(settings.frame_ms)),
            pending: Vec::with_capacity(frame * (BACKLOG_FRAMES + 1)),
            out: Vec::with_capacity(frame),
            next_due: None,
        }
    }

    /// Adds interleaved samples in the frame's channel count, dropping the oldest beyond the
    /// backlog.
    pub fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        let backlog = self.frame * BACKLOG_FRAMES;
        if self.pending.len() > backlog {
            self.pending.drain(..self.pending.len() - backlog);
        }
    }

    /// True once a whole frame is waiting.
    pub fn ready(&self) -> bool {
        self.pending.len() >= self.frame
    }

    /// The latest time to wait until for the next frame, at `now`.
    pub fn deadline(&mut self, now: Instant) -> Instant {
        let due = *self.next_due.get_or_insert(now + self.period);
        if now.saturating_duration_since(due) > RESYNC {
            self.next_due = Some(now);
            return now + SLACK;
        }
        due + SLACK
    }

    /// The next frame: whole if [`Self::ready`], otherwise what has arrived and then silence.
    pub fn take(&mut self) -> &[f32] {
        let count = self.frame.min(self.pending.len());
        self.out.clear();
        self.out.extend(self.pending.drain(..count));
        self.out.resize(self.frame, 0.0);
        if let Some(due) = &mut self.next_due {
            *due += self.period;
        }
        &self.out
    }
}

/// Sound handed over by a system callback (macOS, Linux) to the reading thread.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[derive(Default)]
pub(crate) struct Feed {
    state: Mutex<FeedState>,
    signal: Condvar,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[derive(Default)]
struct FeedState {
    samples: Vec<f32>,
    /// Why capture ended, once it has.
    ended: Option<&'static str>,
    /// The most samples to hold; 0 until the reader says.
    limit: usize,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
impl Feed {
    fn lock(&self) -> std::sync::MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Called by the system's thread with interleaved stereo samples.
    pub(crate) fn push(&self, samples: &[f32]) {
        let mut state = self.lock();
        state.samples.extend_from_slice(samples);
        let limit = state.limit;
        if limit > 0 && state.samples.len() > limit {
            let extra = state.samples.len() - limit;
            state.samples.drain(..extra);
        }
        drop(state);
        self.signal.notify_one();
    }

    /// Called when the system stops the capture.
    pub(crate) fn end(&self, why: &'static str) {
        self.lock().ended.get_or_insert(why);
        self.signal.notify_one();
    }

    /// The next frame for `frames`, waiting at most until it is due.
    pub(crate) fn read<'a>(&self, frames: &'a mut Assembler) -> Result<&'a [f32], MediaError> {
        let mut state = self.lock();
        state.limit = frames.frame * BACKLOG_FRAMES;
        loop {
            frames.push(&state.samples);
            state.samples.clear();
            if let Some(why) = state.ended {
                return Err(MediaError::Capture(why));
            }
            let now = Instant::now();
            let deadline = frames.deadline(now);
            if frames.ready() || now >= deadline {
                break;
            }
            state = self
                .signal
                .wait_timeout(state, deadline - now)
                .map(|(state, _)| state)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        drop(state);
        Ok(frames.take())
    }
}

/// Appends `samples` in `channels` interleaved channels to `out` as stereo: mono is doubled,
/// and channels past the first two are dropped.
pub fn push_stereo(samples: &[f32], channels: usize, out: &mut Vec<f32>) {
    match channels {
        0 => {}
        1 => out.extend(samples.iter().flat_map(|s| [*s, *s])),
        2 => out.extend_from_slice(samples),
        n => out.extend(samples.chunks_exact(n).flat_map(|c| [c[0], c[1]])),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn frames() -> Assembler {
        Assembler::new(&AudioSettings::share_sound())
    }

    #[test]
    fn whole_frames_come_out_as_they_went_in() {
        let mut frames = frames();
        assert_eq!(frames.frame, 1_920, "20 ms of stereo at 48 kHz");
        let sound: Vec<f32> = (0..3_000).map(|i| i as f32).collect();
        frames.push(&sound);
        assert!(frames.ready());
        assert_eq!(frames.take(), &sound[..1_920]);
        assert!(!frames.ready());
    }

    #[test]
    fn an_overdue_frame_is_padded_with_silence() {
        let mut frames = frames();
        frames.push(&[0.5; 100]);
        let out = frames.take();
        assert_eq!(out.len(), 1_920);
        assert_eq!(&out[..100], &[0.5; 100]);
        assert!(out[100..].iter().all(|s| *s == 0.0));
        assert_eq!(frames.take(), &[0.0; 1_920], "nothing playing is silence");
    }

    #[test]
    fn the_backlog_keeps_the_newest_sound() {
        let mut frames = frames();
        let sound: Vec<f32> = (0..1_920 * 8).map(|i| i as f32).collect();
        frames.push(&sound);
        assert_eq!(frames.pending.len(), 1_920 * BACKLOG_FRAMES);
        assert_eq!(frames.take()[0], (1_920 * 3) as f32);
    }

    #[test]
    fn frames_fall_due_in_real_time_and_resync_after_a_stall() {
        let mut frames = frames();
        let start = Instant::now();
        let first = frames.deadline(start);
        assert_eq!(first, start + Duration::from_millis(20) + SLACK);
        frames.take();
        assert_eq!(frames.deadline(start), start + Duration::from_millis(40) + SLACK);
        let late = start + Duration::from_secs(2);
        assert_eq!(frames.deadline(late), late + SLACK, "no burst to catch up");
    }

    #[test]
    fn the_feed_hands_over_sound_and_ends_with_the_reason() {
        let feed = Feed::default();
        let mut frames = frames();
        feed.push(&[0.25; 1_920]);
        assert_eq!(feed.read(&mut frames).unwrap(), &[0.25; 1_920]);
        feed.end("stopped");
        assert_eq!(
            feed.read(&mut frames).err(),
            Some(MediaError::Capture("stopped"))
        );
    }

    #[test]
    fn the_feed_gives_silence_when_nothing_plays() {
        let feed = Feed::default();
        let mut frames = frames();
        let start = Instant::now();
        assert_eq!(feed.read(&mut frames).unwrap(), &[0.0; 1_920]);
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn any_layout_becomes_stereo() {
        let mut out = Vec::new();
        push_stereo(&[0.1, 0.2], 1, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2]);
        out.clear();
        push_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3, &mut out);
        assert_eq!(out, [1.0, 2.0, 4.0, 5.0]);
        out.clear();
        push_stereo(&[1.0, 2.0], 2, &mut out);
        assert_eq!(out, [1.0, 2.0]);
    }
}
