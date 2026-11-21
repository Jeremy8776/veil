//! Automatic microphone volume: brings speech to one steady level, whatever the microphone.
//!
//! Purpose: microphones differ by 30 dB or more. A quiet one (measured 20/11/2026: speech at
//! about -45 dBFS) is hard to hear and never lights the speaking border. [`AutoGain`] follows
//! how loud speech is and turns it up or down towards [`TARGET_DB`], as Zoom, Teams and Krisp
//! do. It learns only from sound the noise model ([`crate::Denoiser`]) scores as speech, so
//! silence and background noise are never pumped up, and the boost fades out when speech stops.
//!
//! Invariants:
//! - The gain stays within [`MIN_GAIN_DB`]..=[`MAX_GAIN_DB`] and moves smoothly: up by at most
//!   [`RISE_DB_PER_S`], down by at most [`FALL_DB_PER_S`].
//! - No frame leaves louder than [`CEILING`], counting any volume applied after this one
//!   ([`AutoGain::set_after`]), so a mic volume above 100% cannot make it clip: a frame that would is turned down at once. Only
//!   a speech frame (a shout) lowers the learnt gain; a loud noise (a door, a key, a cough)
//!   is limited for that frame alone, so it never leaves the voice quiet afterwards.
//! - Without speech the learnt gain holds where it was, and the boost fades to unity within
//!   [`CLOSE_S`]: a frame the model
//!   does not score as speech gets at most the boost scaled by how recently speech was heard
//!   (opens in about [`OPEN_S`], fades over [`CLOSE_S`]), so pauses and noise leave close to
//!   unity. A cut (negative gain) stays on, since it never makes noise louder.
//! - The applied gain ramps across each frame, so a change never clicks.
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
/// How fast the boost comes back when speech starts: within a frame or two.
pub const OPEN_S: f32 = 0.01;
/// How slowly the boost fades once speech stops, so gaps between words are not pumped.
pub const CLOSE_S: f32 = 0.4;
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
    /// How much of a boost is applied, 0 to 1: follows the voice score.
    open: f32,
    /// The linear gain applied at the end of the last frame, where the next ramp starts.
    last: f32,
    /// Highest sample out of this stage: [`CEILING`] divided by any boost applied after it.
    limit: f32,
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
            open: 0.0,
            last: 1.0,
            limit: CEILING,
        }
    }

    /// Tells the stage the gain applied after it (the manual mic volume), so its output still
    /// stays under [`CEILING`] once that is applied. A gain of 1 or less changes nothing.
    pub fn set_after(&mut self, gain: f32) {
        let gain = if gain.is_finite() { gain.max(1.0) } else { 1.0 };
        self.limit = CEILING / gain;
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
        let (towards, tau) = if voice >= VOICE {
            (1.0, OPEN_S)
        } else {
            (0.0, CLOSE_S)
        };
        self.open += (towards - self.open) * (1.0 - (-seconds / tau).exp());
        let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let mut end = amplitude(self.applied_db());
        let mut start = self.last;
        if peak > 0.0 && peak * end > self.limit {
            // This frame would clip: turn it down now.
            let fits = db(self.limit / peak).max(MIN_GAIN_DB);
            if voice >= VOICE {
                // A shout: keep the lower gain, as speech taught it.
                self.gain_db = fits;
                end = amplitude(self.applied_db());
            } else {
                // A noise: this frame only.
                end = amplitude(fits);
            }
            start = start.min(end);
        }
        let step = (end - start) / frame.len() as f32;
        for (i, sample) in frame.iter_mut().enumerate() {
            let gain = start + step * (i + 1) as f32;
            *sample = (*sample * gain).clamp(-self.limit, self.limit);
        }
        self.last = end;
    }

    /// The gain to apply now, in dB: a boost only as far as speech is heard, a cut always.
    fn applied_db(&self) -> f32 {
        if self.gain_db > 0.0 {
            self.gain_db * self.open
        } else {
            self.gain_db
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
    fn pauses_and_noise_are_not_boosted() {
        let mut agc = AutoGain::new();
        speak(&mut agc, -45.0, 4);
        let learnt = agc.gain_db();
        assert!(learnt > 20.0, "{learnt}");
        // Two seconds of room noise the model scores as not speech.
        let mut last = Vec::new();
        for f in 0..100 {
            last = tone(-70.0, f * 960);
            agc.process(&mut last, 0.1);
        }
        let level = rms_db(&last);
        assert!(level < -67.0, "noise left at {level} dBFS");
        assert_eq!(agc.gain_db(), learnt, "the learnt gain is kept");
        // Speech comes back at the learnt level within a few frames.
        let mut back = Vec::new();
        for f in 0..5 {
            back = tone(-45.0, f * 960);
            agc.process(&mut back, 1.0);
        }
        let level = rms_db(&back);
        assert!((level - TARGET_DB).abs() < 3.0, "speech at {level} dBFS");
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
    fn a_loud_noise_does_not_leave_the_voice_quiet() {
        let mut agc = AutoGain::new();
        speak(&mut agc, -45.0, 4);
        let learnt = agc.gain_db();
        // A door right after speech, while the boost is still open.
        let mut door = tone(-6.0, 0);
        agc.process(&mut door, 0.1);
        assert!(door.iter().all(|s| s.abs() <= CEILING));
        assert_eq!(agc.gain_db(), learnt, "the learnt gain is kept");
        let out = speak(&mut agc, -45.0, 1);
        let level = rms_db(&out);
        assert!((level - TARGET_DB).abs() < 3.0, "speech at {level} dBFS");
    }

    #[test]
    fn a_mic_volume_above_100_percent_cannot_make_it_clip() {
        let mut agc = AutoGain::new();
        agc.set_after(1.5);
        speak(&mut agc, -45.0, 4);
        let mut shout = tone(-3.0, 0);
        agc.process(&mut shout, 1.0);
        assert!(shout.iter().all(|s| (s * 1.5).abs() <= CEILING + 1e-6));
        agc.set_after(f32::NAN);
        assert_eq!(agc.limit, CEILING);
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
