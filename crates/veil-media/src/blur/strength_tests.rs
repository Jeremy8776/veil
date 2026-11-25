//! Blur strength: 0% leaves the picture as it was, 100% is the full blur, and values between
//! land between the two.

use super::grid::{Pipeline, Strength};
use super::*;
use crate::effects::{FULL, mix};

const W: usize = 320;
const H: usize = 180;

/// A 320x180 NV12 picture of sharp vertical stripes, so blurring changes it.
fn stripes() -> Vec<u8> {
    let mut d = vec![128; W * (H + H / 2)];
    for y in 0..H {
        for x in 0..W {
            d[y * W + x] = if (x / 3) % 2 == 0 { 30 } else { 220 };
        }
    }
    d
}

/// Composites `orig` with an all-background mask at `strength`.
fn composite(orig: &[u8], strength: u8) -> Vec<u8> {
    let mask = vec![0u8; 256 * 144];
    let mut p = Pipeline::new(W, H);
    let mut d = orig.to_vec();
    p.downscale(&d, W);
    p.blur(
        Strength {
            radius: 4,
            passes: 3,
        },
        &mask,
    );
    p.composite(&mut d, W, &mask, strength);
    d
}

#[test]
fn zero_strength_composite_is_unchanged_and_full_is_blurred() {
    let orig = stripes();
    assert_eq!(composite(&orig, 0), orig);
    assert_ne!(composite(&orig, FULL), orig);
}

#[test]
fn half_strength_lies_between_original_and_full() {
    let orig = stripes();
    let full = composite(&orig, FULL);
    let half = composite(&orig, 50);
    for i in 0..W * H {
        let want = mix(orig[i], full[i], 50);
        assert!(half[i].abs_diff(want) <= 2, "pixel {i}: {} vs {want}", half[i]);
    }
}

#[test]
fn zero_strength_engine_leaves_picture_and_worker_alone() {
    let called = Arc::new(Mutex::new(0u32));
    let seen = Arc::clone(&called);
    let mut e = BlurEngine::with_segmenter(move |_, m| {
        *lock(&seen) += 1;
        m.fill(0);
    });
    let orig = stripes();
    for _ in 0..5 {
        let mut d = orig.clone();
        let status = e.apply(&mut d, W, H, W, Blur::Strong, 0);
        assert_eq!(status, BlurStatus::Blurred);
        assert_eq!(d, orig);
    }
    drop(e);
    assert_eq!(*lock(&called), 0, "no picture reached the segmenter");
}
