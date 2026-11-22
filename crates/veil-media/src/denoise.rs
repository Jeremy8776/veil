//! Noise suppression for speech: keeps the voice, removes fans, keyboards, traffic and hum.
//!
//! Purpose: a [`Denoiser`] cleans 48 kHz mono sound in place, one 10 ms block at a time, with
//! RNNoise (a small recurrent neural network trained on speech and noise) through the pure Rust
//! port `nnnoiseless`. The app runs one on the microphone before the volume, the meter and the
//! call, and optionally one per person on the sound it receives. This is the same idea as
//! Krisp: a model on this device, so nothing leaves it and nobody else needs the feature.
//!
//! Invariants:
//! - Only frames that are a whole number of 10 ms blocks (480 samples) are changed. Every Opus
//!   frame length Veil uses (10, 20, 40, 60 ms) is one. Any other frame is left as it was.
//! - The output lags the input by one block (10 ms), the model's look-back window.
//! - Output stays in -1..1. A non-finite input sample is treated as silence, so one bad sample
//!   cannot poison the network's state for the rest of the call.
//! - One `Denoiser` per stream: it remembers the stream's recent sound.

use nnnoiseless::DenoiseState;

/// Samples in one block the model processes: 10 ms at 48 kHz.
pub const BLOCK: usize = nnnoiseless::FRAME_SIZE;

/// The model expects 16-bit sample values held in `f32`; Veil's sound is -1..1.
const SCALE: f32 = 32_767.0;

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    input: [f32; BLOCK],
    output: [f32; BLOCK],
    /// How sure the model was that the last frame held speech: its best block, 0 to 1.
    voice: f32,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; BLOCK],
            output: [0.0; BLOCK],
            voice: 0.0,
        }
    }

    /// Cleans `frame` (48 kHz mono, -1..1) in place. Returns false, leaving it as it was, if
    /// its length is not a whole number of [`BLOCK`]s.
    pub fn process(&mut self, frame: &mut [f32]) -> bool {
        if frame.is_empty() || !frame.len().is_multiple_of(BLOCK) {
            return false;
        }
        self.voice = 0.0;
        for chunk in frame.as_chunks_mut::<BLOCK>().0 {
            for (into, sample) in self.input.iter_mut().zip(chunk.iter()) {
                *into = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0) * SCALE
                } else {
                    0.0
                };
            }
            let voice = self.state.process_frame(&mut self.output, &self.input);
            self.voice = self.voice.max(voice);
            for (sample, out) in chunk.iter_mut().zip(self.output.iter()) {
                *sample = (out / SCALE).clamp(-1.0, 1.0);
            }
        }
        true
    }

    /// How sure the model was that the last frame held speech (its best 10 ms), 0 to 1.
    pub fn voice(&self) -> f32 {
        self.voice
    }

    /// Forgets the stream so far, for when cleaning starts again after a pause.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        let sum: f32 = samples.iter().map(|s| s * s).sum();
        (sum / samples.len() as f32).sqrt()
    }

    /// Two seconds of fan-like noise, about -31 dBFS, the same every run: white noise through
    /// a one-pole low-pass near 3 kHz. Full-band white noise is a poor stand-in, because the
    /// model's bands stop at 20 kHz and leave anything above that as it was.
    fn rumble() -> Vec<f32> {
        let mut seed = 0x2545_F491_u32;
        let mut low = 0.0f32;
        (0..96_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let white = (f32::from((seed >> 16) as u16) / 32_768.0 - 1.0) * 0.1;
                low += 0.35 * (white - low);
                low
            })
            .collect()
    }

    #[test]
    fn steady_background_noise_is_mostly_removed() {
        let original = rumble();
        let mut cleaned = original.clone();
        let mut denoiser = Denoiser::new();
        for frame in cleaned.as_chunks_mut::<960>().0 {
            assert!(denoiser.process(frame));
        }
        // By the last half second the model has settled on what is noise. Measured: about 4%
        // of the level is left (-28 dB); a quarter (-12 dB) would still be a clear failure.
        let before = rms(&original[72_000..]);
        let after = rms(&cleaned[72_000..]);
        assert!(after < before * 0.25, "noise {before} became {after}");
    }

    #[test]
    fn silence_stays_silent() {
        let mut frame = vec![0.0f32; 960];
        let mut denoiser = Denoiser::new();
        for _ in 0..10 {
            assert!(denoiser.process(&mut frame));
            assert!(frame.iter().all(|s| s.abs() < 1e-3));
        }
    }

    #[test]
    fn frames_that_are_not_whole_blocks_are_left_alone() {
        let mut denoiser = Denoiser::new();
        let mut odd = vec![0.25f32; BLOCK + 1];
        assert!(!denoiser.process(&mut odd));
        assert!(odd.iter().all(|s| *s == 0.25));
        assert!(!denoiser.process(&mut []));
    }

    #[test]
    fn bad_samples_do_not_poison_the_stream() {
        let mut denoiser = Denoiser::new();
        let mut bad = vec![f32::NAN; 960];
        bad[1] = f32::INFINITY;
        bad[2] = 40.0;
        assert!(denoiser.process(&mut bad));
        let mut next = rumble();
        for frame in next.as_chunks_mut::<960>().0.iter_mut().take(10) {
            denoiser.process(frame);
            assert!(frame.iter().all(|s| s.is_finite() && (-1.0..=1.0).contains(s)));
        }
    }
}
