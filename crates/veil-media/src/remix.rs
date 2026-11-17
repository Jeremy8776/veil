//! Changes the channel count of interleaved audio.
//!
//! Purpose: a device may run with a different channel count from the call (a stereo USB
//! microphone, a 6-channel output). Calls are mono or stereo ([`crate::Channels`]), so audio
//! is mixed down to it on the way in and spread out to the device on the way out.
//!
//! Invariants:
//! - Mixing down averages the device channels for each output channel, so loudness never
//!   rises and clipping is never introduced.
//! - Spreading out copies each call channel to the device channels it covers; spare device
//!   channels get silence, never noise.
//! - A trailing partial frame (fewer samples than one frame of channels) is dropped.

/// Appends `input` (interleaved, `from` channels) to `out` with `to` channels.
pub fn remix(input: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
    if from == 0 || to == 0 {
        return;
    }
    if from == to {
        let whole = input.len() / from * from;
        out.extend_from_slice(&input[..whole]);
        return;
    }
    for frame in input.chunks_exact(from) {
        if to < from {
            // Output channel c averages the device channels mapped to it: c, c + to, ...
            for c in 0..to {
                let (sum, count) = frame
                    .iter()
                    .skip(c)
                    .step_by(to)
                    .fold((0.0f32, 0u16), |(s, n), v| (s + v, n + 1));
                out.push(sum / f32::from(count.max(1)));
            }
        } else if from == 1 {
            // Mono to many: the same sample in the first two channels (left and right).
            for c in 0..to {
                out.push(if c < 2 { frame[0] } else { 0.0 });
            }
        } else {
            for c in 0..to {
                out.push(frame.get(c).copied().unwrap_or(0.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::remix;

    fn mix(input: &[f32], from: usize, to: usize) -> Vec<f32> {
        let mut out = Vec::new();
        remix(input, from, to, &mut out);
        out
    }

    #[test]
    fn same_count_copies_whole_frames() {
        assert_eq!(mix(&[0.1, 0.2, 0.3], 2, 2), [0.1, 0.2]);
    }

    #[test]
    fn stereo_to_mono_averages() {
        assert_eq!(mix(&[1.0, 0.0, 0.5, 0.5], 2, 1), [0.5, 0.5]);
    }

    #[test]
    fn six_channels_to_stereo_averages_alternate_channels() {
        assert_eq!(mix(&[0.75, 0.0, 0.25, 0.0, 0.5, 0.75], 6, 2), [0.5, 0.25]);
    }

    #[test]
    fn mono_spreads_to_left_and_right_only() {
        assert_eq!(mix(&[0.5], 1, 2), [0.5, 0.5]);
        assert_eq!(mix(&[0.5], 1, 4), [0.5, 0.5, 0.0, 0.0]);
    }

    #[test]
    fn stereo_to_more_fills_spare_channels_with_silence() {
        assert_eq!(mix(&[0.1, 0.2], 2, 3), [0.1, 0.2, 0.0]);
    }

    #[test]
    fn zero_channels_add_nothing() {
        assert!(mix(&[0.1], 0, 1).is_empty());
        assert!(mix(&[0.1], 1, 0).is_empty());
    }
}
