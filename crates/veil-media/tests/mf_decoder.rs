//! Decodes what the Media Foundation encoders produce on this machine, with Microsoft's H.264
//! decoder. Skips, with a note, when no encoder or decoder is available.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use veil_media::{
    EncodedLayer, Ladder, MediaError, MfDecoder, RawPicture, Schedule, Tier, VideoDecoder,
    VideoEncoder, media_foundation,
};

const W: usize = 1_920;
const H: usize = 1_080;

/// A moving gradient, as in the encoder test.
fn picture(n: u64) -> Vec<u8> {
    let mut data = vec![128u8; W * H * 3 / 2];
    for y in 0..H {
        for x in 0..W {
            data[y * W + x] = ((x + y + n as usize * 4) % 256) as u8;
        }
    }
    data
}

fn open_decoder() -> Option<MfDecoder> {
    match MfDecoder::open() {
        Ok(decoder) => Some(decoder),
        Err(MediaError::Unavailable(what)) => {
            eprintln!("skipped: no {what} on this machine");
            None
        }
        Err(e) => panic!("open decoder: {e}"),
    }
}

/// Encodes `count` pictures on the Thumb and Hd rungs, with a keyframe requested at picture 13
/// (it comes out at 32, as in the encoder test). `None` when there is no encoder.
fn encode(hardware: bool, count: u64) -> Option<Vec<(u64, Vec<EncodedLayer>)>> {
    let ladder = Ladder::camera().up_to(Tier::Hd);
    let mut encoder = match media_foundation(&ladder, hardware) {
        Ok(encoder) => encoder,
        Err(MediaError::Unavailable(what)) => {
            eprintln!("skipped: no {what} on this machine");
            return None;
        }
        Err(e) => panic!("open encoder: {e}"),
    };
    let mut schedule = Schedule::new(ladder);
    let start = Instant::now();
    let mut out = Vec::new();
    for n in 0..count {
        if n == 13 {
            schedule.request_keyframe();
        }
        let plan = schedule.next(start + Duration::from_millis(17 * n));
        let data = picture(n);
        let raw = RawPicture::new(W as u16, H as u16, W, 17_000 * n, &data).unwrap();
        out.push((n, encoder.encode(&raw, &plan).unwrap()));
    }
    Some(out)
}

/// Mean absolute luma difference between a decoded Hd picture and its source.
fn luma_error(decoded: &RawPicture<'_>, n: u64) -> f64 {
    let source = picture(n);
    let total: u64 = decoded.data[..W * H]
        .iter()
        .zip(&source[..W * H])
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    total as f64 / (W * H) as f64
}

fn decode_each_rung(hardware: bool) {
    let Some(mut decoders) = open_decoder().map(|d| [d, MfDecoder::open().unwrap()]) else {
        return;
    };
    let Some(stream) = encode(hardware, 24) else {
        return;
    };
    let mut decoded = [0usize; 2];
    for (n, layers) in &stream {
        for layer in layers {
            let spatial = usize::from(layer.layer.spatial);
            let picture = decoders[spatial]
                .decode(&layer.bitstream, 17_000 * n)
                .unwrap()
                .unwrap_or_else(|| panic!("picture {n} rung {spatial}: no picture out"));
            assert_eq!((picture.width, picture.height), (layer.width, layer.height));
            assert_eq!(
                picture.data.len(),
                picture.width as usize * picture.height as usize * 3 / 2
            );
            assert_eq!(
                picture.timestamp_us,
                17_000 * n,
                "picture {n} rung {spatial}"
            );
            if spatial == 1 {
                let error = luma_error(&picture, *n);
                assert!(
                    error < 6.0,
                    "picture {n}: luma differs by {error:.2} on average"
                );
            }
            decoded[spatial] += 1;
        }
    }
    // Low latency: every access unit gave its picture straight away. Thumb runs at 15 fps and
    // Hd at 30 from 60 fps capture.
    assert_eq!(decoded, [6, 12]);
}

#[test]
fn decodes_software_encoder_output() {
    decode_each_rung(false);
}

#[test]
fn decodes_hardware_encoder_output() {
    decode_each_rung(true);
}

#[test]
fn follows_a_switch_of_rung_at_a_keyframe() {
    let Some(mut decoder) = open_decoder() else {
        return;
    };
    let Some(stream) = encode(false, 34) else {
        return;
    };
    // Thumb until picture 32, the next keyframe, then Hd: as after a subscription raise.
    for (n, layers) in &stream {
        let rung = u8::from(*n >= 32);
        let Some(layer) = layers.iter().find(|l| l.layer.spatial == rung) else {
            continue;
        };
        let picture = decoder
            .decode(&layer.bitstream, 17_000 * n)
            .unwrap()
            .unwrap();
        let want = if rung == 0 {
            (640, 360)
        } else {
            (1_920, 1_080)
        };
        assert_eq!((picture.width, picture.height), want, "picture {n}");
    }
}

#[test]
fn bad_input_is_an_error_or_no_picture() {
    let Some(mut decoder) = open_decoder() else {
        return;
    };
    assert!(decoder.decode(&[], 0).is_err());
    // A P slice with no keyframe before it, then bytes that are not H.264 at all.
    let p_slice = [0, 0, 0, 1, 0x41, 0x9a, 0x00, 0x10, 0x20];
    let noise: Vec<u8> = (0..4_000u32).map(|i| (i * 7919 % 251) as u8).collect();
    for input in [&p_slice[..], &noise] {
        if let Ok(Some(picture)) = decoder.decode(input, 0) {
            panic!(
                "a picture from bad input: {}x{}",
                picture.width, picture.height
            );
        }
    }
    // The decoder still works after bad input, starting at a keyframe (Thumb: pictures 0, 4).
    let Some(stream) = encode(false, 8) else {
        return;
    };
    let mut got = 0;
    for (n, layers) in &stream {
        let Some(thumb) = layers.iter().find(|l| l.layer.spatial == 0) else {
            continue;
        };
        if decoder.decode(&thumb.bitstream, *n).unwrap().is_some() {
            got += 1;
        }
    }
    assert_eq!(got, 2);
}
