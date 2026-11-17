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
//! - A speaker change bumps a counter; the playback thread compares it each 20 ms frame and
//!   reopens the device when it moved. The lock is held only to copy the id.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::media::lock;

/// Highest microphone gain: 200%, for a quiet microphone.
pub const MIC_MAX: f32 = 2.0;
/// Highest speaker gain: 100%. Louder than the call's own mix only clips.
pub const SPEAKER_MAX: f32 = 1.0;

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

/// The speaker the call plays through, and how loud.
#[derive(Default)]
pub struct Playback {
    /// Device id; `None` is the system default.
    device: Mutex<Option<String>>,
    changes: AtomicU64,
    pub volume: Gain,
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
