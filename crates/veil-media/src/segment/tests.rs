//! Tests for the segmentation network: weights hash, parity with onnxruntime, API contract.
//!
//! Purpose: show the hand-written network computes what the published model computes.
//!
//! Invariants:
//! - `parity_logits.bin` comes from `tools/selfie_weights.py` (onnxruntime on the ONNX model,
//!   final sigmoid removed) and holds every third row and column of the H x W logits as f32.
//! - The test picture is built here with the same integer arithmetic as the tool. It is
//!   synthetic; no photograph is used anywhere.

use super::{MODEL_H as H, MODEL_W as W, Segmenter};
use sha2::{Digest, Sha256};

const GRID_STEP: usize = 3;
const WEIGHTS_SHA256: &str = "aafbc18409206308925f21703011617ce407067dd195bb69e4594c271b409228";
/// Largest allowed absolute difference in a logit. Logits span about +-200, so this is
/// a relative error near 1e-5.
const TOLERANCE: f32 = 1e-3;

/// The tool's `test_picture()`: planar CHW in [0, 1].
fn test_input() -> Vec<f32> {
    let mut s: u32 = 0x1234_5678;
    let mut noise = Vec::with_capacity(H * W * 3);
    for _ in 0..H * W * 3 {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        noise.push((s >> 24) as i32);
    }
    let mut chw = vec![0.0f32; 3 * H * W];
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let mut px = [
                60 + x * 40 / W as i64,
                90 + y * 40 / H as i64,
                120 - x * 30 / W as i64,
            ];
            if ((x - 128) * 70).pow(2) + ((y - 150) * 80).pow(2) < (80 * 70i64).pow(2) {
                px = [40, 60, 110];
            }
            if ((x - 128) * 42).pow(2) + ((y - 55) * 34).pow(2) < (34 * 42i64).pow(2) {
                px = [205, 150, 120];
            }
            let i = (y * W as i64 + x) as usize;
            for (c, v) in px.iter().enumerate() {
                let n = i64::from(noise[i * 3 + c] >> 2) - 32;
                chw[c * H * W + i] = (v + n).clamp(0, 255) as f32 / 255.0;
            }
        }
    }
    chw
}

#[test]
fn weights_match_published_hash() {
    let digest = Sha256::digest(super::net::WEIGHTS);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, WEIGHTS_SHA256);
}

#[test]
fn logits_match_onnxruntime() {
    let reference: Vec<f32> = include_bytes!("parity_logits.bin")
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let mut seg = Segmenter::new();
    let input = test_input();
    let logits = seg.logits(&input).map(<[f32]>::to_vec).unwrap_or_default();
    assert_eq!(logits.len(), H * W);
    let cols = W.div_ceil(GRID_STEP);
    assert_eq!(reference.len(), H.div_ceil(GRID_STEP) * cols);
    let mut worst = 0.0f32;
    for (i, &want) in reference.iter().enumerate() {
        let (y, x) = (i / cols * GRID_STEP, i % cols * GRID_STEP);
        worst = worst.max((logits[y * W + x] - want).abs());
    }
    println!("max abs logit difference: {worst:.3e}");
    assert!(worst <= TOLERANCE, "max abs logit difference {worst}");
    // The reference must be a real test: both signs, wide spread.
    assert!(reference.iter().any(|&v| v > 10.0) && reference.iter().any(|&v| v < -10.0));
}

#[test]
fn wrong_lengths_leave_mask_untouched() {
    let mut seg = Segmenter::new();
    let mut mask = vec![7u8; H * W];
    seg.run(&vec![0.5; 3 * H * W - 1], &mut mask);
    assert!(mask.iter().all(|&m| m == 7));
    let mut short = vec![7u8; H * W - 1];
    seg.run(&vec![0.5; 3 * H * W], &mut short);
    assert!(short.iter().all(|&m| m == 7));
}

#[test]
fn mask_has_person_and_background() {
    let mut seg = Segmenter::new();
    let mut mask = vec![0u8; H * W];
    seg.run(&test_input(), &mut mask);
    let person = mask.iter().filter(|&&m| m > 200).count();
    let background = mask.iter().filter(|&&m| m < 55).count();
    // The head and body ellipses are a bit under half the picture.
    assert!(person > H * W / 10, "person pixels: {person}");
    assert!(background > H * W / 10, "background pixels: {background}");
}

/// `cargo test -p veil-media --release segment_timing -- --ignored --nocapture`
#[test]
#[ignore = "benchmark"]
fn segment_timing() {
    let mut seg = Segmenter::new();
    let input = test_input();
    let mut mask = vec![0u8; H * W];
    for _ in 0..30 {
        seg.run(&input, &mut mask);
    }
    let mut ms = Vec::with_capacity(600);
    for _ in 0..600 {
        let t = std::time::Instant::now();
        seg.run(&input, &mut mask);
        ms.push(t.elapsed().as_secs_f64() * 1e3);
        std::hint::black_box(&mask);
    }
    ms.sort_by(f64::total_cmp);
    println!(
        "segment run ms: median {:.3}  p95 {:.3}  max {:.3}",
        ms[300], ms[569], ms[599]
    );
}
