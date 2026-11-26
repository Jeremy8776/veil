//! Microphone and speaker volume, and which speaker plays the call.
//!
//! Purpose: [`Playback`] is shared by the interface (pre-join and in-call device pickers) and
//! the call's playback thread (`engine/audio_out.rs`), so a different speaker or volume takes
//! effect during a call without leaving it. [`scale`] applies a volume to samples.
//!
//! Invariants:
//! - Volumes are software gains: the microphone from 0 to [`MIC_MAX`], the speaker from 0 to
//!   [`SPEAKER_MAX`]. The device's own volume (in the operating system) is never changed.
//! - Scaled samples are clamped to -1..1, so a boost clips rather than wraps.
//! - Each share's computer sound has its own volume and mute ([`ShareLevel`]), on this device
//!   only: the sharer's own level is theirs, and nobody else's changes. Read every 20 ms.
//! - Each person's voice has its own volume (0 to [`VOICE_MAX`]) and mute the same way, on
//!   this device only. It never mutes them for anyone else.
//! - A speaker change bumps a counter; the playback thread compares it each 20 ms frame and
//!   reopens the device when it moved. The lock is held only to copy the id.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::media::lock;

/// Highest microphone gain: 200%, for a quiet microphone.
pub const MIC_MAX: f32 = 2.0;
/// Highest speaker gain: 100%. Louder than the call's own mix only clips.
pub const SPEAKER_MAX: f32 = 1.0;
/// Highest gain for one person's voice: 200%, so a quiet person can be turned up.
pub const VOICE_MAX: f32 = 2.0;

/// Multiplies every sample by `gain` and clamps the result to -1..1.
pub fn scale(samples: &mut [f32], gain: f32) {
    let gain = if gain.is_finite() { gain.max(0.0) } else { 1.0 };
    for sample in samples {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
}

/// A gain held as `f32` bits, for threads that read it every frame. Starts at 100%.
pub struct Gain(AtomicU32);

impl Default for Gain {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl Gain {
    pub fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    /// Stores `value` clamped to 0..=`max`; anything not a number is ignored.
    pub fn set(&self, value: f32, max: f32) {
        if value.is_finite() {
            self.0
                .store(value.clamp(0.0, max).to_bits(), Ordering::Relaxed);
        }
    }
}

/// How loud one share's computer sound, or one person's voice, plays here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShareLevel {
    /// 0 to [`SPEAKER_MAX`].
    pub volume: f32,
    pub muted: bool,
}

impl Default for ShareLevel {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

impl ShareLevel {
    /// The gain to play at: 0 while muted.
    pub fn gain(self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }
}

/// The speaker the call plays through, and how loud.
#[derive(Default)]
pub struct Playback {
    /// Device id; `None` is the system default.
    device: Mutex<Option<String>>,
    changes: AtomicU64,
    pub volume: Gain,
    /// Remove background noise from everyone else's sound, one model per person.
    /// shortcut: forgotten when Veil closes; save it once the app has saved preferences.
    pub clean_others: AtomicBool,
    /// Each sharer's computer sound, by participant id; absent is [`ShareLevel::default`].
    shares: Mutex<HashMap<u32, ShareLevel>>,
    /// Each person's voice, by participant id; absent is [`ShareLevel::default`].
    voices: Mutex<HashMap<u32, ShareLevel>>,
}

/// `level` with its volume clamped to 0 to `max`; anything not a number counts as 100%.
fn clamped(level: ShareLevel, max: f32) -> ShareLevel {
    let volume = if level.volume.is_finite() {
        level.volume.clamp(0.0, max)
    } else {
        1.0
    };
    ShareLevel { volume, ..level }
}

impl Playback {
    /// Chooses another speaker; the playback thread switches on its next frame.
    pub fn set_device(&self, id: Option<String>) {
        let mut device = lock(&self.device);
        if *device != id {
            *device = id;
            self.changes.fetch_add(1, Ordering::Release);
        }
    }

    pub fn device(&self) -> Option<String> {
        lock(&self.device).clone()
    }

    pub fn share_level(&self, id: u32) -> ShareLevel {
        lock(&self.shares).get(&id).copied().unwrap_or_default()
    }

    /// Sets how loud `id`'s share sound plays here. The volume is clamped to 0 to
    /// [`SPEAKER_MAX`]; anything not a number counts as 100%.
    pub fn set_share_level(&self, id: u32, level: ShareLevel) {
        lock(&self.shares).insert(id, clamped(level, SPEAKER_MAX));
    }

    pub fn voice_level(&self, id: u32) -> ShareLevel {
        lock(&self.voices).get(&id).copied().unwrap_or_default()
    }

    /// Sets how loud `id`'s voice plays here, 0 to [`VOICE_MAX`]. Nobody else hears a change.
    pub fn set_voice_level(&self, id: u32, level: ShareLevel) {
        lock(&self.voices).insert(id, clamped(level, VOICE_MAX));
    }

    /// Moves on every change of device.
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_boosts_cuts_and_clips() {
        let mut s = [0.25, -0.25, 0.75, -0.75];
        scale(&mut s, 2.0);
        assert_eq!(s, [0.5, -0.5, 1.0, -1.0]);
        scale(&mut s, 0.0);
        assert_eq!(s, [0.0; 4]);
        let mut t = [0.5];
        scale(&mut t, f32::NAN);
        assert_eq!(t, [0.5], "a bad gain leaves sound as it is");
        scale(&mut t, -3.0);
        assert_eq!(t, [0.0], "a negative gain is silence, not an inverted wave");
    }

    #[test]
    fn gains_stay_in_range() {
        let gain = Gain::new(1.0);
        gain.set(5.0, MIC_MAX);
        assert_eq!(gain.get(), MIC_MAX);
        gain.set(-1.0, MIC_MAX);
        assert_eq!(gain.get(), 0.0);
        gain.set(f32::INFINITY, MIC_MAX);
        assert_eq!(gain.get(), 0.0, "ignored");
    }

    #[test]
    fn each_voice_has_its_own_level_here() {
        let playback = Playback::default();
        assert_eq!(playback.voice_level(5), ShareLevel::default());
        let loud = ShareLevel {
            volume: 9.0,
            muted: false,
        };
        playback.set_voice_level(5, loud);
        assert_eq!(playback.voice_level(5).volume, VOICE_MAX, "clamped");
        assert_eq!(
            playback.voice_level(6),
            ShareLevel::default(),
            "others unchanged"
        );
        assert_eq!(
            playback.share_level(5),
            ShareLevel::default(),
            "share sound apart"
        );
        let muted = ShareLevel {
            volume: 1.0,
            muted: true,
        };
        playback.set_voice_level(5, muted);
        assert_eq!(playback.voice_level(5).gain(), 0.0);
    }

    #[test]
    fn each_share_has_its_own_level_here() {
        let playback = Playback::default();
        assert_eq!(playback.share_level(3), ShareLevel::default());
        playback.set_share_level(
            3,
            ShareLevel {
                volume: 0.4,
                muted: true,
            },
        );
        playback.set_share_level(
            4,
            ShareLevel {
                volume: 9.0,
                muted: false,
            },
        );
        assert_eq!(playback.share_level(3).gain(), 0.0, "muted");
        assert_eq!(playback.share_level(3).volume, 0.4, "kept for unmuting");
        assert_eq!(playback.share_level(4).gain(), SPEAKER_MAX);
        assert_eq!(
            playback.share_level(5),
            ShareLevel::default(),
            "others untouched"
        );
    }

    #[test]
    fn only_a_real_change_of_speaker_counts() {
        let playback = Playback::default();
        assert_eq!(playback.volume.get(), 1.0);
        playback.set_device(None);
        assert_eq!(playback.changes(), 0);
        playback.set_device(Some("headset".into()));
        playback.set_device(Some("headset".into()));
        assert_eq!(playback.changes(), 1);
        assert_eq!(playback.device().as_deref(), Some("headset"));
    }
}
