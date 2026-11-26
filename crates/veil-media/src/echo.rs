//! Echo cancellation: removes what Veil played from what the microphone hears.
//!
//! Purpose: an [`EchoCanceller`] takes the sound written to the speaker ([`EchoCanceller::played`])
//! and the microphone's sound ([`EchoCanceller::clean`]), and subtracts the first from the second,
//! so other people do not hear themselves back. It is WebRTC's AEC3 through the pure Rust port
//! `sonora`, with only echo cancellation on: Veil's own noise removal and automatic volume
//! (`Denoiser`, `AutoGain`) run after it. ADR 0009 says why.
//!
//! Invariants:
//! - 48 kHz mono, -1..1, both sides. Only frames that are a whole number of 10 ms blocks
//!   ([`BLOCK`]) are used; any other frame is ignored (`played`) or left as it was (`clean`).
//! - A non-finite sample is treated as silence, so one bad sample cannot poison the filter.
//! - The delay between playing and hearing is estimated by AEC3 itself; none is set.
//! - One `EchoCanceller` per microphone and speaker pair: it learns their echo path.

use sonora::config::{EchoCanceller as AecConfig, MaxProcessingRate, Pipeline};
use sonora::{AudioProcessing, Config, StreamConfig};

use crate::audio::SAMPLE_RATE;

/// Samples in one block AEC3 processes: 10 ms at 48 kHz.
pub const BLOCK: usize = SAMPLE_RATE as usize / 100;

pub struct EchoCanceller {
    apm: AudioProcessing,
    input: [f32; BLOCK],
    output: [f32; BLOCK],
}

impl Default for EchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoCanceller {
    pub fn new() -> Self {
        let stream = StreamConfig::new(SAMPLE_RATE, 1);
        let config = Config {
            // The default 32 kHz drops everything above 16 kHz; Opus at 48 kHz carries it.
            pipeline: Pipeline {
                maximum_internal_processing_rate: MaxProcessingRate::Rate48kHz,
                ..Pipeline::default()
            },
            echo_canceller: Some(AecConfig::default()),
            ..Config::default()
        };
        Self {
            apm: AudioProcessing::builder()
                .config(config)
                .capture_config(stream)
                .render_config(stream)
                .build(),
            input: [0.0; BLOCK],
            output: [0.0; BLOCK],
        }
    }

    /// Tells the canceller what was just written to the speaker. Returns false, using none of
    /// it, if its length is not a whole number of [`BLOCK`]s.
    pub fn played(&mut self, frame: &[f32]) -> bool {
        if frame.is_empty() || !frame.len().is_multiple_of(BLOCK) {
            return false;
        }
        for chunk in frame.as_chunks::<BLOCK>().0 {
            copy_clean(&mut self.input, chunk);
            // Errors are only for unsupported formats, which the fixed config rules out.
            let _ = self
                .apm
                .process_render_f32(&[&self.input], &mut [&mut self.output]);
        }
        true
    }

    /// Removes echo from `frame` (the microphone) in place. Returns false, leaving it as it
    /// was, if its length is not a whole number of [`BLOCK`]s.
    pub fn clean(&mut self, frame: &mut [f32]) -> bool {
        if frame.is_empty() || !frame.len().is_multiple_of(BLOCK) {
            return false;
        }
        for chunk in frame.as_chunks_mut::<BLOCK>().0 {
            copy_clean(&mut self.input, chunk);
            if self
                .apm
                .process_capture_f32(&[&self.input], &mut [&mut self.output])
                .is_ok()
            {
                for (sample, out) in chunk.iter_mut().zip(self.output.iter()) {
                    *sample = out.clamp(-1.0, 1.0);
                }
            }
        }
        true
    }

    /// Forgets the echo path, for a new device or when cancelling starts again.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

fn copy_clean(into: &mut [f32; BLOCK], from: &[f32; BLOCK]) {
    for (to, sample) in into.iter_mut().zip(from.iter()) {
        *to = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 ms, as Veil's frames.
    const FRAME: usize = 2 * BLOCK;

    /// Speech-like test sound: noise with a slow syllable-rate envelope, from a fixed seed.
    struct Talker {
        seed: u32,
        t: usize,
        rate: f32,
    }

    impl Talker {
        fn new(seed: u32, rate: f32) -> Self {
            Self { seed, t: 0, rate }
        }

        fn next(&mut self) -> f32 {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 17;
            self.seed ^= self.seed << 5;
            let noise = (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
            let phase = self.t as f32 / SAMPLE_RATE as f32 * self.rate * std::f32::consts::TAU;
            self.t += 1;
            noise * 0.3 * (0.6 + 0.4 * phase.sin())
        }
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|s| s * s).sum()
    }

    fn db(ratio: f32) -> f32 {
        10.0 * ratio.log10()
    }

    /// Plays `seconds` of far-end sound; the mic hears it 60 ms later at 0.3, plus near-end
    /// sound from `near`. Returns (mic energy, cleaned energy) over the last two seconds.
    fn run(seconds: usize, far_on: bool, mut near: impl FnMut() -> f32) -> (f32, f32) {
        let delay = SAMPLE_RATE as usize * 60 / 1000;
        let mut aec = EchoCanceller::new();
        let mut far = Talker::new(1, 4.0);
        let mut line = vec![0.0f32; delay];
        let frames = seconds * SAMPLE_RATE as usize / FRAME;
        let tail = frames - 2 * SAMPLE_RATE as usize / FRAME;
        let (mut before, mut after) = (0.0, 0.0);
        for n in 0..frames {
            let played: Vec<f32> = (0..FRAME)
                .map(|_| if far_on { far.next() } else { 0.0 })
                .collect();
            line.extend_from_slice(&played);
            let mut mic: Vec<f32> = line
                .drain(..FRAME)
                .map(|echo| 0.3 * echo + near())
                .collect();
            assert!(aec.played(&played));
            if n >= tail {
                before += energy(&mic);
            }
            assert!(aec.clean(&mut mic));
            if n >= tail {
                after += energy(&mic);
            }
        }
        (before, after)
    }

    #[test]
    fn echo_drops_by_twenty_db() {
        let mut hiss = Talker::new(7, 0.0);
        let (before, after) = run(6, true, || hiss.next() * 0.003);
        let drop = db(before / after);
        assert!(drop >= 20.0, "echo dropped only {drop:.1} dB");
    }

    #[test]
    fn near_end_speech_passes_when_nothing_plays() {
        let mut voice = Talker::new(3, 3.0);
        let (before, after) = run(4, false, || voice.next());
        let change = db(before / after).abs();
        assert!(change <= 1.0, "near-end changed by {change:.1} dB");
    }

    #[test]
    fn odd_lengths_are_left_alone() {
        let mut aec = EchoCanceller::new();
        let mut frame = vec![0.5f32; BLOCK + 1];
        assert!(!aec.played(&frame));
        assert!(!aec.clean(&mut frame));
        assert!(frame.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn bad_samples_become_silence() {
        let mut aec = EchoCanceller::new();
        let mut frame = vec![f32::NAN; FRAME];
        assert!(aec.played(&frame));
        assert!(aec.clean(&mut frame));
        assert!(frame.iter().all(|s| s.is_finite()));
    }
}
