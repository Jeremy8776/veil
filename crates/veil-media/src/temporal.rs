//! The dyadic temporal pattern: which frame-rate step each picture belongs to.
//!
//! Purpose: with `levels` temporal levels, every `2^(levels - 1)`th picture is temporal layer
//! 0, the pictures halfway between are layer 1, and so on. Dropping the top layer halves the
//! frame rate, and nothing in a lower layer refers to a higher one, so a forwarder can drop
//! higher layers without breaking the lower ones.
//!
//! Example, 3 levels (period 4): phases 0, 1, 2, 3 are layers 0, 2, 1, 2. At 60 fps capture,
//! layer 0 alone is 15 fps, layers 0 and 1 are 30 fps, all three are 60 fps.

use veil_security::layer::MAX_TEMPORAL;

/// Most temporal levels a stream may use.
pub const MAX_LEVELS: u8 = MAX_TEMPORAL + 1;

/// Pictures in one repeat of the pattern.
pub fn period(levels: u8) -> u64 {
    1 << levels.clamp(1, MAX_LEVELS).saturating_sub(1)
}

/// The temporal layer of the picture at `phase` pictures after the last keyframe.
pub fn temporal_id(phase: u64, levels: u8) -> u8 {
    let levels = levels.clamp(1, MAX_LEVELS);
    let p = phase % period(levels);
    if p == 0 {
        return 0;
    }
    // p is below 8, so trailing_zeros is at most 2 and fits a u8.
    let zeros = u8::try_from(p.trailing_zeros()).unwrap_or(0);
    (levels - 1).saturating_sub(zeros)
}

/// The frame rate of layers 0 to `top` when pictures are captured at `capture_fps`.
pub fn rate(capture_fps: u32, levels: u8, top: u8) -> u32 {
    let levels = levels.clamp(1, MAX_LEVELS);
    let dropped = (levels - 1).saturating_sub(top);
    capture_fps >> dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_levels_follow_the_dyadic_pattern() {
        let ids: Vec<u8> = (0..8).map(|p| temporal_id(p, 3)).collect();
        assert_eq!(ids, [0, 2, 1, 2, 0, 2, 1, 2]);
    }

    #[test]
    fn four_levels_and_one_level() {
        let ids: Vec<u8> = (0..8).map(|p| temporal_id(p, 4)).collect();
        assert_eq!(ids, [0, 3, 2, 3, 1, 3, 2, 3]);
        assert!((0..8).all(|p| temporal_id(p, 1) == 0));
    }

    #[test]
    fn rates_halve_per_dropped_layer() {
        assert_eq!(rate(60, 3, 2), 60);
        assert_eq!(rate(60, 3, 1), 30);
        assert_eq!(rate(60, 3, 0), 15);
        assert_eq!(period(3), 4);
    }
}
