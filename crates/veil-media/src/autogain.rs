//! Automatic microphone volume: brings speech to one steady level, whatever the microphone.
//!
//! Purpose: microphones differ by 30 dB or more. A quiet one (measured 20/11/2026: speech at
//! about -45 dBFS) is hard to hear and never lights the speaking border. [`AutoGain`] follows
//! how loud speech is and turns it up or down towards [`TARGET_DB`], as Zoom, Teams and Krisp
//! do. It learns only from sound the noise model ([`crate::Denoiser`]) scores as speech, so
//! silence and background noise are never pumped up.
//!
//! Invariants:
//! - The gain stays within [`MIN_GAIN_DB`]..=[`MAX_GAIN_DB`] and moves smoothly: up by at most
//!   [`RISE_DB_PER_S`], down by at most [`FALL_DB_PER_S`].
//! - No frame leaves louder than [`CEILING`]: a frame that would is turned down at once, and
//!   the gain learns from it.
//! - Without speech the gain holds where it was.
//! - Non-finite samples are treated as silence.

use crate::audio::SAMPLE_RATE;

/// Where the loud part of speech should sit: RMS of a loud 20 ms frame, in dBFS.
pub const TARGET_DB: f32 = -18.0;
/// Most boost: 30 dB (about x32). The quiet microphone measured needed about 27 dB.
pub const MAX_GAIN_DB: f32 = 30.0;
/// Most cut: 12 dB, for a microphone that is far too hot.
pub const MIN_GAIN_DB: f32 = -12.0;
/// How fast the gain may rise. Slow, so a pause between sentences is not boosted.
pub const RISE_DB_PER_S: f32 = 10.0;
/// How fast the gain may fall. Fast, so a shout is tamed within a word.
pub const FALL_DB_PER_S: f32 = 40.0;
/// Highest sample after the gain.
pub const CEILING: f32 = 0.9;
/// A frame counts as speech above this voice score from the noise model.
pub const VOICE: f32 = 0.6;
/// Frames quieter than this are not learnt from, even if scored as speech.
const FLOOR_DB: f32 = -70.0;
/// Time constants of the speech level estimate: quick to rise, slow to fall, so it follows
/// the loud part of speech rather than the gaps between words.
const LEVEL_ATTACK_S: f32 = 0.05;
const LEVEL_RELEASE_S: f32 = 1.5;

pub struct AutoGain {
    /// Loudness of recent speech, dBFS; `None` until speech is first heard.
    level_db: Option<f32>,
    gain_db: f32,
}

impl Default for AutoGain {
    fn default() -> Self {
        Self::new()
    }
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-6).log10()
}

fn amplitude(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl AutoGain {
    pub fn new() -> Self {
        Self {
            level_db: None,
            gain_db: 0.0,
        }
    }

    /// The gain now, in dB.
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Applies the gain to `frame` (48 kHz mono, -1..1) in place. `voice` is the noise model's
    /// speech score for the same frame, 0 to 1.
    pub fn process(&mut self, frame: &mut [f32], voice: f32) {
        if frame.is_empty() {
            return;
        }
        for sample in frame.iter_mut() {
            if !sample.is_finite() {
                *sample = 0.0;
            }
        }
        let seconds = frame.len() as f32 / SAMPLE_RATE as f32;
        let sum: f32 = frame.iter().map(|s| s * s).sum();
        let frame_db = db((sum / frame.len() as f32).sqrt());
        if voice >= VOICE && frame_db > FLOOR_DB {
            let level = self.level_db.get_or_insert(frame_db);
            let tau = if frame_db > *level {
                LEVEL_ATTACK_S
            } else {
                LEVEL_RELEASE_S
            };
            *level += (frame_db - *level) * (1.0 - (-seconds / tau).exp());
            let wanted = (TARGET_DB - *level).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            let step = wanted - self.gain_db;
            self.gain_db += step.clamp(-FALL_DB_PER_S * seconds, RISE_DB_PER_S * seconds);
        }
        let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > 0.0 && peak * amplitude(self.gain_db) > CEILING {
            // This frame would clip: turn down now, and keep the lower gain.
            self.gain_db = db(CEILING / peak).max(MIN_GAIN_DB);
        }
        let gain = amplitude(self.gain_db);
        for sample in frame.iter_mut() {
            *sample = (*sample * gain).clamp(-CEILING, CEILING);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 ms of a 200 Hz tone whose RMS is `rms_db` dBFS.
    fn tone(rms_db: f32, start: usize) -> Vec<f32> {
        let amp = amplitude(rms_db) * std::f32::consts::SQRT_2;
        (start..start + 960)
            .map(|i| amp * (i as f32 * 200.0 * std::f32::consts::TAU / 48_000.0).sin())
            .collect()
    }

    fn rms_db(frame: &[f32]) -> f32 {
        let sum: f32 = frame.iter().map(|s| s * s).sum();
        db((sum / frame.len() as f32).sqrt())
    }

    /// Runs `seconds` of speech at `level` dBFS and returns the last frame out.
    fn speak(agc: &mut AutoGain, level: f32, seconds: usize) -> Vec<f32> {
        let mut last = Vec::new();
        for f in 0..seconds * 50 {
            last = tone(level, f * 960);
            agc.process(&mut last, 1.0);
        }
        last
    }

    #[test]
    fn a_quiet_microphone_is_brought_up_to_the_target() {
        let mut agc = AutoGain::new();
        // The quiet microphone measured on 20/11/2026.
        let out = speak(&mut agc, -45.0, 4);
        let level = rms_db(&out);
        assert!((level - TARGET_DB).abs() < 3.0, "speech at {level} dBFS");
    }

    #[test]
    fn a_loud_microphone_is_turned_down() {
        let mut agc = AutoGain::new();
        let out = speak(&mut agc, -6.0, 2);
        let level = rms_db(&out);
        assert!((level - TARGET_DB).abs() < 3.0, "speech at {level} dBFS");
    }

    #[test]
    fn the_boost_rises_gradually_and_never_passes_its_limit() {
        let mut agc = AutoGain::new();
        speak(&mut agc, -45.0, 1);
        assert!(agc.gain_db() <= RISE_DB_PER_S + 0.01, "{}", agc.gain_db());
        speak(&mut agc, -80.0 + 11.0, 10);
        assert!(agc.gain_db() <= MAX_GAIN_DB, "{}", agc.gain_db());
    }

    #[test]
    fn silence_and_noise_are_not_learnt_from() {
        let mut agc = AutoGain::new();
        for f in 0..500 {
            // Loud-ish sound the model scored as not speech.
            let mut frame = tone(-50.0, f * 960);
            agc.process(&mut frame, 0.1);
        }
        assert_eq!(agc.gain_db(), 0.0);
    }

    #[test]
    fn a_sudden_shout_does_not_clip() {
        let mut agc = AutoGain::new();
        speak(&mut agc, -45.0, 4);
        let mut shout = tone(-3.0, 0);
        agc.process(&mut shout, 1.0);
        assert!(shout.iter().all(|s| s.abs() <= CEILING));
        assert!(agc.gain_db() < 0.0, "learnt from the shout");
    }

    #[test]
    fn bad_samples_become_silence() {
        let mut agc = AutoGain::new();
        let mut frame = vec![f32::NAN, f32::INFINITY, 0.1];
        agc.process(&mut frame, 1.0);
        assert!(frame.iter().all(|s| s.is_finite()));
        assert_eq!(frame[0], 0.0);
    }
}
