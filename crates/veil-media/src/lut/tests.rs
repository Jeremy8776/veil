//! Parser tests, including hostile input, and picture tests for the Y,U,V table.
#![allow(clippy::unwrap_used)]

use super::cube::{self, LutError, MAX_FILE, MAX_GRID};
use super::*;
use crate::effects::{FULL, mix};

/// A `.cube` of `size` points per side, each output from `f(r, g, b)` in 0..1.
fn cube_text(size: usize, f: impl Fn(f32, f32, f32) -> [f32; 3]) -> String {
    let mut s = format!("TITLE \"test\"\nLUT_3D_SIZE {size}\n");
    let step = (size - 1) as f32;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let [x, y, z] = f(r as f32 / step, g as f32 / step, b as f32 / step);
                s.push_str(&format!("{x:.6} {y:.6} {z:.6}\n"));
            }
        }
    }
    s
}

fn identity_text(size: usize) -> String {
    cube_text(size, |r, g, b| [r, g, b])
}

/// A 64x32 NV12 picture with varied luma and chroma, tightly packed, inside the video range.
fn picture() -> Vec<u8> {
    let (w, h) = (64, 32);
    let mut d = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        for x in 0..w {
            d.push((40 + (x * 3 + y * 2) % 160) as u8);
        }
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            d.push((100 + (x * 2 + y) % 56) as u8);
            d.push((156 - (x + y * 3) % 56) as u8);
        }
    }
    d
}

#[test]
fn parses_a_simple_cube() {
    let c = cube::parse(identity_text(2).as_bytes()).unwrap();
    assert_eq!(c.size, 2);
    assert_eq!(c.table.len(), 8);
    assert_eq!(c.table[1], [1.0, 0.0, 0.0], "red changes fastest");
    assert_eq!(c.domain_max, [1.0; 3]);
}

#[test]
fn accepts_comments_bom_crlf_domain_and_input_range() {
    let text = "\u{feff}# made by hand\r\nLUT_3D_SIZE 2\r\nDOMAIN_MIN 0 0 0\r\nDOMAIN_MAX 1 1 1\r\n\r\n\
                0 0 0\r\n1 0 0\r\n0 1 0\r\n1 1 0\r\n0 0 1\r\n1 0 1\r\n0 1 1\r\n1 1 1\r\n";
    assert!(cube::parse(text.as_bytes()).is_ok());
    let range = identity_text(2).replace("TITLE \"test\"", "LUT_3D_INPUT_RANGE 0.0 1.0");
    assert!(cube::parse(range.as_bytes()).is_ok());
}

#[test]
fn refuses_hostile_and_malformed_files() {
    let ok = identity_text(2);
    let last = "1.000000 1.000000 1.000000";
    let cases: Vec<(String, LutError)> = vec![
        (String::new(), LutError::NoSize),
        ("0 0 0\n".into(), LutError::NoSize),
        ("LUT_1D_SIZE 4\n".into(), LutError::OneDimensional),
        ("LUT_3D_SIZE 1\n".into(), LutError::BadSize),
        (format!("LUT_3D_SIZE {}\n", MAX_GRID + 1), LutError::BadSize),
        (
            "LUT_3D_SIZE 99999999999999999999\n".into(),
            LutError::BadSize,
        ),
        ("LUT_3D_SIZE -2\n".into(), LutError::BadSize),
        (ok.replace(last, "1 1"), LutError::BadValue(10)),
        (ok.replace(last, "1 1 1 1"), LutError::BadLine(10)),
        (ok.replace(last, "1 NaN 1"), LutError::BadValue(10)),
        (ok.replace(last, "1 inf 1"), LutError::BadValue(10)),
        (ok.replace(last, "1 9 1"), LutError::BadValue(10)),
        (ok.replace(last, "1 0x1 1"), LutError::BadValue(10)),
        (ok.replace(&format!("{last}\n"), ""), LutError::WrongCount),
        (format!("{ok}0 0 0\n"), LutError::WrongCount),
        (format!("{ok}TITLE \"late\"\n"), LutError::Misplaced(11)),
        (
            ok.replace("TITLE \"test\"", "LUT_3D_SIZE 2"),
            LutError::Misplaced(2),
        ),
        (
            ok.replace("TITLE \"test\"", "EVIL_KEYWORD 1"),
            LutError::BadLine(1),
        ),
        (
            ok.replace("TITLE \"test\"", "DOMAIN_MIN 1 1 1\nDOMAIN_MAX 0 0 0"),
            LutError::BadDomain,
        ),
        (
            ok.replace("TITLE \"test\"", &"#".repeat(300)),
            LutError::BadLine(1),
        ),
    ];
    for (text, want) in cases {
        assert_eq!(cube::parse(text.as_bytes()), Err(want.clone()), "{want:?}");
        assert!(!want.to_string().is_empty());
    }
    assert_eq!(cube::parse(&[0xff, 0xfe, 0x00]), Err(LutError::NotText));
    assert_eq!(
        cube::parse(&vec![b'#'; MAX_FILE + 1]),
        Err(LutError::TooLarge)
    );
}

#[test]
fn garbage_never_panics() {
    let alphabet = b"0123456789 .-\nLUT_3DSIZE#e";
    let mut seed = 7u32;
    for len in [0, 1, 17, 300, 5000] {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                alphabet[(seed >> 16) as usize % alphabet.len()]
            })
            .collect();
        let _ = cube::parse(&bytes);
        let _ =
            cube::parse(format!("LUT_3D_SIZE 2\n{}", String::from_utf8_lossy(&bytes)).as_bytes());
    }
}

/// The RGB (0 to 255) the preview shows for each pixel of a 64x32 `picture`.
fn shown(d: &[u8]) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(64 * 32);
    for y in 0..32 {
        for x in 0..64 {
            let c = 64 * 32 + (y / 2) * 64 + (x / 2) * 2;
            let rgb = super::table::yuv_to_rgb(
                f32::from(d[y * 64 + x]),
                f32::from(d[c]),
                f32::from(d[c + 1]),
            );
            out.push(rgb.map(|v| v.clamp(0.0, 1.0) * 255.0));
        }
    }
    out
}

fn worst(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
        .fold(0.0, f32::max)
}

#[test]
fn identity_lut_keeps_what_is_shown() {
    let lut = load(identity_text(17).as_bytes()).unwrap();
    let orig = picture();
    let mut d = orig.clone();
    lut.apply(&mut d, 64, 32, 64);
    let w = worst(&shown(&d), &shown(&orig));
    assert!(w <= 6.0, "worst difference {w}");
    let mut exact = orig.clone();
    YuvLut::identity().apply(&mut exact, 64, 32, 64);
    assert_eq!(exact, orig, "the identity grid is exact");
}

#[test]
fn invert_lut_inverts_and_mixes_by_strength() {
    let lut = load(cube_text(9, |r, g, b| [1.0 - r, 1.0 - g, 1.0 - b]).as_bytes()).unwrap();
    let orig = picture();
    let mut full = orig.clone();
    lut.apply(&mut full, 64, 32, 64);
    let want: Vec<[f32; 3]> = shown(&orig).iter().map(|p| p.map(|v| 255.0 - v)).collect();
    let w = worst(&shown(&full), &want);
    assert!(w <= 8.0, "worst difference {w}");
    let mut none = orig.clone();
    lut.mixed(0).apply(&mut none, 64, 32, 64);
    assert_eq!(none, orig, "0% is the original");
    let mut all = orig.clone();
    lut.mixed(FULL).apply(&mut all, 64, 32, 64);
    assert_eq!(all, full, "100% is the full look");
    let mut half = orig.clone();
    lut.mixed(50).apply(&mut half, 64, 32, 64);
    for i in 0..orig.len() {
        let want = mix(orig[i], full[i], 50);
        assert!(
            half[i].abs_diff(want) <= 1,
            "byte {i}: {} vs {want}",
            half[i]
        );
    }
}

#[test]
fn black_and_white_lut_neutralises_chroma() {
    let grey = cube_text(5, |r, g, b| {
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        [l, l, l]
    });
    let lut = load(grey.as_bytes()).unwrap();
    let mut d = picture();
    lut.apply(&mut d, 64, 32, 64);
    for &c in &d[64 * 32..] {
        assert!(c.abs_diff(128) <= 1, "chroma {c}");
    }
}

/// Every `.cube` in `VEIL_LUT_DIR`, such as a folder of real looks, parses and applies. Skipped
/// when the variable is not set.
#[test]
fn real_luts_from_a_folder() {
    let Some(dir) = std::env::var_os("VEIL_LUT_DIR") else {
        return;
    };
    let (mut ok, mut refused) = (0, Vec::new());
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let is_cube = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cube"));
        if !is_cube {
            continue;
        }
        match load(&std::fs::read(&path).unwrap()) {
            Ok(lut) => {
                let mut d = picture();
                lut.apply(&mut d, 64, 32, 64);
                ok += 1;
            }
            Err(e) => refused.push(format!("{}: {e}", path.display())),
        }
    }
    println!("{ok} loaded, {} refused", refused.len());
    for r in &refused {
        println!("  {r}");
    }
}

#[test]
fn bad_sizes_and_short_buffers_are_left_alone() {
    let lut = load(cube_text(3, |r, g, b| [b, g, r]).as_bytes()).unwrap();
    for (w, h, stride) in [(0, 2, 4), (3, 2, 4), (4, 3, 4), (8, 2, 4)] {
        let mut d = vec![50u8; 24];
        lut.apply(&mut d, w, h, stride);
        assert_eq!(d, vec![50u8; 24], "{w}x{h}/{stride}");
    }
    for len in 0..12 {
        let mut d = vec![50u8; len];
        lut.apply(&mut d, 4, 2, 4);
        assert_eq!(d, vec![50u8; len]);
    }
}

/// Frame cost at 1080p. Run with `--release -- --ignored --nocapture` for the real number.
#[test]
#[ignore = "timing; run by hand in release"]
fn cost_at_1080p() {
    let lut = load(cube_text(33, |r, g, b| [g, b, r]).as_bytes()).unwrap();
    let (w, h) = (1920, 1080);
    let mut d: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i * 7 % 251) as u8).collect();
    let runs = 60;
    let start = std::time::Instant::now();
    for _ in 0..runs {
        lut.apply(&mut d, w, h, w);
    }
    let lut_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
    let start = std::time::Instant::now();
    for _ in 0..runs {
        crate::effects::apply_filter(&mut d, w, h, w, crate::Filter::Warm, 60);
    }
    let filter_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
    let start = std::time::Instant::now();
    let mixed = lut.mixed(60);
    let mix_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = std::time::Instant::now();
    let _ = load(cube_text(65, |r, g, b| [g, b, r]).as_bytes());
    let import_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "1080p: LUT {lut_ms:.2} ms/frame, filter {filter_ms:.2} ms/frame, \
         strength change {mix_ms:.2} ms, 65-point import {import_ms:.0} ms"
    );
    assert_eq!(mixed.mixed(FULL), mixed);
}

#[test]
fn an_identity_ramp_is_the_greys_themselves() {
    let ramp = load_ramp(identity_text(17).as_bytes()).unwrap();
    for (out, grey) in ramp.iter().zip(RAMP) {
        for c in out {
            assert!((c - grey).abs() < 1e-4, "{out:?} for {grey}");
        }
    }
}

#[test]
fn a_ramp_shows_split_toning() {
    // Teal shadows, orange highlights: blue up below mid-grey, red up above it.
    let text = cube_text(9, |r, g, b| {
        let l = (r + g + b) / 3.0;
        let t = (l - 0.5) * 0.4;
        [r + t, g, b - t]
    });
    let ramp = load_ramp(text.as_bytes()).unwrap();
    let (shadow, highlight) = (ramp[0], ramp[RAMP.len() - 1]);
    assert!(shadow[2] > shadow[0], "shadows lean blue: {shadow:?}");
    assert!(
        highlight[0] > highlight[2],
        "highlights lean red: {highlight:?}"
    );
    assert!(ramp.iter().flatten().all(|c| (0.0..=1.0).contains(c)));
}

#[test]
fn a_ramp_reads_a_cube_with_its_own_domain() {
    let mut text = String::from("DOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n");
    text.push_str(
        &cube_text(5, |r, g, b| [r * 0.5, g * 0.5, b * 0.5]).replace("TITLE \"test\"\n", ""),
    );
    let ramp = load_ramp(text.as_bytes()).unwrap();
    // Half of each grey: the domain is mapped, so 0.5 reaches the cube's middle.
    assert!((ramp[2][0] - 0.25).abs() < 1e-3, "{:?}", ramp[2]);
}
