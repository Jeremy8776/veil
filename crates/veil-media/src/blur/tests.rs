use super::grid::{Pipeline, Strength};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PAD: u8 = 0xAA;

/// NV12 picture with detail (a noisy pattern) and `PAD` in the stride padding.
fn picture(w: usize, h: usize, stride: usize) -> Vec<u8> {
    let mut d = vec![PAD; stride * (h + h / 2)];
    let mut s = 12345u32;
    let mut noise = || {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        (s >> 24) as u8
    };
    for row in 0..h + h / 2 {
        for x in 0..w {
            d[row * stride + x] = 40 + noise() % 160;
        }
    }
    d
}

fn constant(v: u8) -> impl FnMut(&[f32], &mut [u8]) + Send + 'static {
    move |_, m| m.fill(v)
}

/// Applies copies of `orig` until the result stops being blurred or time runs out.
fn settle(e: &mut BlurEngine, orig: &[u8], w: usize, h: usize, stride: usize) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        let mut d = orig.to_vec();
        e.apply(&mut d, w, h, stride, Blur::Strong);
        if d == orig {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

fn roughness(d: &[u8], w: usize, h: usize, stride: usize) -> u64 {
    let mut t = 0u64;
    for y in 0..h {
        for x in 1..w {
            t += u64::from(d[y * stride + x].abs_diff(d[y * stride + x - 1]));
        }
    }
    t
}

#[test]
fn all_person_leaves_picture_unchanged() {
    for stride in [320, 352] {
        let orig = picture(320, 180, stride);
        let mut e = BlurEngine::with_segmenter(constant(255));
        assert!(settle(&mut e, &orig, 320, 180, stride), "stride {stride}");
    }
}

#[test]
fn all_background_changes_picture_and_keeps_padding() {
    let (w, h, stride) = (320, 180, 352);
    let orig = picture(w, h, stride);
    let mut e = BlurEngine::with_segmenter(constant(0));
    for _ in 0..20 {
        let mut d = orig.clone();
        assert_eq!(
            e.apply(&mut d, w, h, stride, Blur::Light),
            BlurStatus::Blurred
        );
        assert_ne!(d, orig);
        assert!(roughness(&d, w, h, stride) < roughness(&orig, w, h, stride) / 2);
        for row in 0..h + h / 2 {
            assert!(
                d[row * stride + w..(row + 1) * stride]
                    .iter()
                    .all(|&b| b == PAD)
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn before_first_mask_whole_picture_is_blurred() {
    let (tx, rx) = mpsc::channel::<()>();
    let mut e = BlurEngine::with_segmenter(move |_, _| {
        let _ = rx.recv();
    });
    let orig = picture(320, 180, 320);
    let mut d = orig.clone();
    assert_eq!(
        e.apply(&mut d, 320, 180, 320, Blur::Strong),
        BlurStatus::Blurred
    );
    assert!(roughness(&d, 320, 180, 320) < roughness(&orig, 320, 180, 320) / 2);
    drop(tx);
}

#[test]
fn slow_segmenter_gives_too_busy_and_never_blocks() {
    let (tx, rx) = mpsc::channel::<()>();
    let mut first = true;
    let mut e = BlurEngine::with_segmenter(move |_, m| {
        if first {
            first = false;
            m.fill(255);
        } else {
            let _ = rx.recv(); // blocks until the test drops `tx`
        }
    });
    let orig = picture(320, 180, 320);
    assert!(settle(&mut e, &orig, 320, 180, 320), "first mask arrives");
    let start = Instant::now();
    let mut busy_at = None;
    for i in 0..12 {
        let mut d = orig.clone();
        let t = Instant::now();
        let st = e.apply(&mut d, 320, 180, 320, Blur::Light);
        assert!(t.elapsed() < Duration::from_millis(200), "apply blocked");
        if st == BlurStatus::TooBusy {
            assert_eq!(d, orig, "TooBusy leaves the picture");
            busy_at.get_or_insert(i);
        } else {
            assert!(busy_at.is_none(), "stays busy");
        }
    }
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(
        busy_at.is_some_and(|i| i <= MAX_MASK_AGE as usize + 1),
        "{busy_at:?}"
    );
    drop(tx);
}

struct Flag(Arc<AtomicBool>);
impl Drop for Flag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn dropping_engine_joins_worker() {
    let done = Arc::new(AtomicBool::new(false));
    let guard = Flag(Arc::clone(&done));
    let mut e = BlurEngine::with_segmenter(move |_, m| {
        let _keep = &guard;
        m.fill(0);
    });
    e.apply(&mut picture(64, 48, 64), 64, 48, 64, Blur::Light);
    drop(e);
    assert!(
        done.load(Ordering::SeqCst),
        "worker closure dropped, so thread ended"
    );
}

#[test]
fn off_and_malformed_pictures_are_untouched_and_do_not_panic() {
    let mut e = BlurEngine::with_segmenter(constant(0));
    let orig = picture(64, 48, 64);
    let mut d = orig.clone();
    e.apply(&mut d, 64, 48, 64, Blur::Off);
    assert_eq!(d, orig);
    let cases = [
        (0, 0, 0),
        (63, 48, 64),
        (64, 47, 64),
        (64, 48, 60),
        (64, 0, 64),
        (usize::MAX, 4, 4),
    ];
    for (w, h, s) in cases {
        let mut d = orig.clone();
        e.apply(&mut d, w, h, s, Blur::Strong);
        assert_eq!(d, orig, "{w}x{h} stride {s}");
    }
    for len in 0..orig.len() {
        let mut d = orig[..len].to_vec();
        e.apply(&mut d, 64, 48, 64, Blur::Strong);
        assert_eq!(d, orig[..len], "len {len}");
    }
}

#[test]
fn general_path_matches_fast_path_at_720p() {
    let (w, h) = (1280, 720);
    let mut orig = picture(w, h, w);
    // smooth content (as a camera gives) so the two upsamplers agree
    for (i, b) in orig.iter_mut().enumerate() {
        *b = 60 + ((i % w) / 8 + (i / w) / 8) as u8 % 120;
    }
    let mask: Vec<u8> = (0..256 * 144)
        .map(|i| if (i % 256) < 100 { 255 } else { 0 })
        .collect();
    let run = |general: bool| {
        let mut p = Pipeline::new(w, h);
        if general {
            p.force_general();
        }
        let mut d = orig.clone();
        p.downscale(&d, w);
        p.blur(
            Strength {
                radius: 4,
                passes: 3,
            },
            &mask,
        );
        p.composite(&mut d, w, &mask);
        d
    };
    let (a, b) = (run(false), run(true));
    let max = a
        .iter()
        .zip(&b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0);
    assert!(max <= 3, "max diff {max}");
    assert_ne!(a, orig);
}

#[test]
fn other_sizes_work() {
    for (w, h, stride) in [
        (640, 480, 640),
        (640, 480, 704),
        (128, 72, 128),
        (2, 2, 2),
        (250, 140, 256),
    ] {
        let orig = picture(w, h, stride);
        let mut e = BlurEngine::with_segmenter(constant(0));
        let mut d = orig.clone();
        assert_eq!(
            e.apply(&mut d, w, h, stride, Blur::Strong),
            BlurStatus::Blurred
        );
        if w > 4 {
            assert_ne!(d, orig, "{w}x{h}");
        }
    }
}

#[test]
fn fast_segmenter_gives_the_current_pictures_mask() {
    let mut e = BlurEngine::with_segmenter(constant(0));
    let orig = picture(320, 180, 320);
    for _ in 0..10 {
        let mut d = orig.clone();
        e.apply(&mut d, 320, 180, 320, Blur::Light);
    }
    assert_eq!(
        e.mask_age(),
        Some(0),
        "the mask should not trail the picture"
    );
}

#[test]
fn person_colour_does_not_bleed_into_background() {
    // Left half bright (the person), right half dark (the background), as luma; neutral chroma.
    let (w, h) = (1280, 720);
    let mut d = vec![128u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            d[y * w + x] = if x < w / 2 { 220 } else { 30 };
        }
    }
    let mask: Vec<u8> = (0..256 * 144)
        .map(|i| if i % 256 < 128 { 255 } else { 0 })
        .collect();
    let mut p = Pipeline::new(w, h);
    p.downscale(&d, w);
    p.blur(
        Strength {
            radius: 4,
            passes: 3,
        },
        &mask,
    );
    p.composite(&mut d, w, &mask);
    // Background 10 to 40 pixels right of the edge stays dark: no bright halo.
    for x in w / 2 + 10..w / 2 + 40 {
        assert!(d[360 * w + x] <= 34, "halo at x={x}: {}", d[360 * w + x]);
    }
    // The person stays as it was.
    assert!(d[360 * w..360 * w + w / 2 - 10].iter().all(|&v| v == 220));
}

/// How far the mask trails the picture with the real network at camera pace (30 fps):
/// `cargo test -p veil-media --release blur::tests::mask_lag -- --ignored --nocapture`.
#[test]
#[ignore]
fn mask_lag_real_network() {
    let (w, h) = (1280, 720);
    let model_ms = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&model_ms);
    let mut net = crate::segment::Segmenter::new();
    let mut e = BlurEngine::with_segmenter(move |i, m| {
        let t = Instant::now();
        net.run(i, m);
        if let Ok(mut v) = log.lock() {
            v.push(t.elapsed().as_secs_f64() * 1e3);
        }
    });
    let frames: Vec<Vec<u8>> = (0..4).map(|_| picture(w, h, w)).collect();
    let mut buf = frames[0].clone();
    let (mut ages, mut times) = ([0u32; 5], Vec::new());
    for i in 0..330 {
        buf.copy_from_slice(&frames[i % 4]);
        let t = Instant::now();
        e.apply(&mut buf, w, h, w, Blur::Strong);
        if i >= 30 {
            times.push(t.elapsed().as_secs_f64() * 1e3);
            let age = e.mask_age().unwrap_or(4).min(4) as usize;
            ages[age] += 1;
        }
        std::thread::sleep(Duration::from_millis(33));
    }
    times.sort_by(f64::total_cmp);
    println!(
        "mask age in pictures [0, 1, 2, 3, 4+]: {ages:?}; apply median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        times[times.len() - 1]
    );
    if let Ok(mut v) = model_ms.lock() {
        v.sort_by(f64::total_cmp);
        println!(
            "model on worker: median {:.3} ms, p95 {:.3} ms",
            v[v.len() / 2],
            v[v.len() * 95 / 100]
        );
    }
}

/// Release benchmark: `cargo test -p veil-media --release blur::tests::bench -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_apply_720p() {
    let (w, h) = (1280, 720);
    let mask: Vec<u8> = (0..256 * 144)
        .map(|i| {
            let (x, y) = ((i % 256) as f32 - 128.0, (i / 256) as f32 - 72.0);
            if x * x + y * y < 3600.0 { 255 } else { 0 }
        })
        .collect();
    // A fake model as slow as the real one (about 3 ms), so the wait for the mask is counted.
    let mut e = BlurEngine::with_segmenter(move |_, m| {
        std::thread::sleep(Duration::from_millis(3));
        m.copy_from_slice(&mask);
    });
    let frames: Vec<Vec<u8>> = (0..4).map(|_| picture(w, h, w)).collect();
    let mut buf = frames[0].clone();
    let mut times = Vec::new();
    for i in 0..630 {
        buf.copy_from_slice(&frames[i % 4]);
        let t = Instant::now();
        e.apply(&mut buf, w, h, w, Blur::Strong);
        if i >= 30 {
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    times.sort_by(f64::total_cmp);
    println!(
        "apply 1280x720 Strong: median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        times[times.len() - 1]
    );
}
