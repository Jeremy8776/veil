//! Encodes synthetic NV12 pictures through Media Foundation on this machine: the hardware
//! encoder if there is one, and Microsoft's software encoder. Skips, with a note, when no
//! H.264 encoder is available.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use veil_media::annexb::{self, NAL_IDR, NAL_SLICE};
use veil_media::encoder::check_output;
use veil_media::{
    Ladder, MediaError, PicturePlan, RawPicture, Schedule, Tier, VideoEncoder, media_foundation,
};
use veil_security::Wanted;

/// A moving gradient, so the encoder has real work and P pictures are not empty.
fn picture(n: usize) -> Vec<u8> {
    let (width, height) = (1_920, 1_080);
    let mut data = vec![128u8; width * height * 3 / 2];
    for y in 0..height {
        for x in 0..width {
            data[y * width + x] = ((x + y + n * 4) % 256) as u8;
        }
    }
    data
}

/// Encodes one planned picture and checks each layer's bitstream against the plan.
fn encode(encoder: &mut impl VideoEncoder, plan: &PicturePlan, at_us: u64) {
    let data = picture(plan.picture as usize);
    let raw = RawPicture::new(1_920, 1_080, 1_920, at_us, &data).unwrap();
    let out = encoder.encode(&raw, plan).unwrap();
    check_output(plan, &out).unwrap();
    let n = plan.picture;
    for layer in &out {
        let units = annexb::units(&layer.bitstream);
        let types: Vec<u8> = units.iter().map(|u| u[0] & 0x1f).collect();
        if plan.keyframe {
            assert!(
                types.contains(&NAL_IDR),
                "picture {n}: keyframe without IDR: {types:?}"
            );
        } else {
            assert!(
                types.contains(&NAL_SLICE),
                "picture {n}: expected a P slice: {types:?}"
            );
            assert!(
                !types.contains(&NAL_IDR),
                "picture {n}: unplanned IDR: {types:?}"
            );
        }
        // Hd's top temporal layer is 1: nothing may refer to it, so its slices are not
        // reference pictures (nal_ref_idc 0). This is what makes dropping it safe.
        if layer.layer.spatial == 1 && layer.layer.temporal == 1 {
            let refs = units
                .iter()
                .filter(|u| matches!(u[0] & 0x1f, 1 | 5))
                .all(|u| u[0] >> 5 == 0);
            assert!(refs, "picture {n}: temporal 1 on Hd is a reference picture");
        }
        let size = if layer.layer.spatial == 0 {
            (640, 360)
        } else {
            (1_920, 1_080)
        };
        assert_eq!((layer.width, layer.height), size);
    }
}

fn run(hardware: bool) {
    let ladder = Ladder::camera().up_to(Tier::Hd);
    let mut encoder = match media_foundation(&ladder, hardware) {
        Ok(encoder) => encoder,
        Err(MediaError::Unavailable(what)) => {
            eprintln!("skipped: no {what} on this machine");
            return;
        }
        Err(e) => panic!("open: {e}"),
    };
    for rung in encoder.encoders() {
        eprintln!(
            "rung encoder: {} (hardware: {})",
            rung.name(),
            rung.is_hardware()
        );
    }
    let mut schedule = Schedule::new(ladder);
    let start = Instant::now();
    let mut keyframes = Vec::new();
    for n in 0..72u64 {
        let now = start + Duration::from_millis(17 * n);
        match n {
            // A receiver lost the stream; the keyframe waits for the 500 ms gap and the next
            // pattern boundary, and must come out as an IDR on every rung.
            13 => schedule.request_keyframe(),
            // Nobody wants Hd: it pauses at the next temporal 0 picture.
            36 => schedule.set_wanted(Some(Wanted::THUMB)),
            // Hd is wanted again: it restarts at the next keyframe, with a fresh encoder.
            44 => schedule.set_wanted(Some(Wanted::ALL)),
            _ => {}
        }
        let plan = schedule.next(now);
        if plan.keyframe {
            keyframes.push((n, plan.layers.len()));
        }
        encode(&mut encoder, &plan, 17_000 * n);
    }
    // The first picture; the request (gap ends at picture 30, boundary at 32); the resume
    // (gap after picture 32 ends at 62, boundary at 64).
    assert_eq!(keyframes, [(0, 2), (32, 2), (64, 2)]);
}

#[test]
fn hardware_encoder_encodes_and_forces_keyframes() {
    run(true);
}

#[test]
fn software_encoder_encodes_and_forces_keyframes() {
    run(false);
}
