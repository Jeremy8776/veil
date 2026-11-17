//! The three convolution kernels: 1x1 pointwise, dense 3x3 and depthwise.
//!
//! Purpose: these are almost all of the run time. Each is written so the compiler can
//! vectorise the inner loop on baseline x86-64 and aarch64 without any CPU-specific code.
//!
//! Invariants:
//! - Activations are NHWC f32. Weight layouts are those in `tools/selfie_weights.py`.
//! - The activation is a const generic (`A`): 0 none, 1 ReLU, 2 hard-swish, 3 sigmoid, so the
//!   `match` is resolved at compile time and does not sit in the inner loop.
//! - Padding is implicit: a tap outside the picture is skipped (zero padding).

use super::ops::sigmoid;

/// Const-generic activation codes, in step with [`super::net::Act`].
#[inline(always)]
fn act<const A: u8>(x: f32) -> f32 {
    match A {
        1 => x.max(0.0),
        2 => x * (x + 3.0).clamp(0.0, 6.0) * (1.0 / 6.0),
        3 => sigmoid(x),
        _ => x,
    }
}

/// Copies eight values.
#[inline(always)]
fn load8(s: &[f32]) -> [f32; 8] {
    let mut a = [0.0; 8];
    a.copy_from_slice(&s[..8]);
    a
}

/// Pointwise convolution. `w` is `cout / 8` tiles of `cin` rows of 8 output weights. Four
/// pixels share each weight read.
pub(super) fn conv1x1<const A: u8>(
    x: &[f32],
    w: &[f32],
    b: &[f32],
    out: &mut [f32],
    px: usize,
    cin: usize,
    cout: usize,
) {
    let x = &x[..px * cin];
    let out = &mut out[..px * cout];
    let w = &w[..cout * cin];
    let b = &b[..cout];
    let mut p = 0;
    while p + 4 <= px {
        let x0 = &x[p * cin..][..cin];
        let x1 = &x[(p + 1) * cin..][..cin];
        let x2 = &x[(p + 2) * cin..][..cin];
        let x3 = &x[(p + 3) * cin..][..cin];
        for (t, wt) in w.chunks_exact(cin * 8).enumerate() {
            let co = t * 8;
            let bias = load8(&b[co..]);
            let (mut a0, mut a1, mut a2, mut a3) = (bias, bias, bias, bias);
            for (k, wr) in wt.as_chunks::<8>().0.iter().enumerate() {
                let (v0, v1, v2, v3) = (x0[k], x1[k], x2[k], x3[k]);
                for j in 0..8 {
                    a0[j] += v0 * wr[j];
                    a1[j] += v1 * wr[j];
                    a2[j] += v2 * wr[j];
                    a3[j] += v3 * wr[j];
                }
            }
            for (q, a) in [a0, a1, a2, a3].iter().enumerate() {
                let o = &mut out[(p + q) * cout + co..][..8];
                for j in 0..8 {
                    o[j] = act::<A>(a[j]);
                }
            }
        }
        p += 4;
    }
    while p < px {
        let x0 = &x[p * cin..][..cin];
        for (t, wt) in w.chunks_exact(cin * 8).enumerate() {
            let co = t * 8;
            let mut a = load8(&b[co..]);
            for (k, wr) in wt.as_chunks::<8>().0.iter().enumerate() {
                let v = x0[k];
                for j in 0..8 {
                    a[j] += v * wr[j];
                }
            }
            let o = &mut out[p * cout + co..][..8];
            for j in 0..8 {
                o[j] = act::<A>(a[j]);
            }
        }
        p += 1;
    }
}

/// Dense KxK convolution to 16 output channels (the first layer, 3 channels in). `w` is
/// `[ky][kx][cin][16]`; `pt` and `pl` are the top and left padding.
#[allow(clippy::too_many_arguments)] // lean: one flat shape list per op; a struct adds nothing
pub(super) fn conv_dense<const A: u8>(
    x: &[f32],
    w: &[f32],
    b: &[f32],
    out: &mut [f32],
    ih: usize,
    iw: usize,
    oh: usize,
    ow: usize,
    cin: usize,
    k: usize,
    s: usize,
    pt: usize,
    pl: usize,
) {
    const COUT: usize = 16;
    let x = &x[..ih * iw * cin];
    let out = &mut out[..oh * ow * COUT];
    let mut bias = [0.0f32; COUT];
    bias.copy_from_slice(&b[..COUT]);
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = bias;
            for ky in 0..k {
                let iy = (oy * s + ky).wrapping_sub(pt);
                if iy >= ih {
                    continue;
                }
                for kx in 0..k {
                    let ix = (ox * s + kx).wrapping_sub(pl);
                    if ix >= iw {
                        continue;
                    }
                    let xp = &x[(iy * iw + ix) * cin..][..cin];
                    let wp = &w[(ky * k + kx) * cin * COUT..][..cin * COUT];
                    for (xv, wr) in xp.iter().zip(wp.as_chunks::<COUT>().0.iter()) {
                        for j in 0..COUT {
                            acc[j] += xv * wr[j];
                        }
                    }
                }
            }
            let o = &mut out[(oy * ow + ox) * COUT..][..COUT];
            for j in 0..COUT {
                o[j] = act::<A>(acc[j]);
            }
        }
    }
}

/// Depthwise KxK convolution, one filter per channel. `w` is `[ky][kx][c]`.
#[allow(clippy::too_many_arguments)] // lean: as for `conv_dense`
pub(super) fn depthwise<const A: u8>(
    x: &[f32],
    w: &[f32],
    b: &[f32],
    out: &mut [f32],
    ih: usize,
    iw: usize,
    oh: usize,
    ow: usize,
    c: usize,
    k: usize,
    s: usize,
    pt: usize,
    pl: usize,
) {
    let x = &x[..ih * iw * c];
    let out = &mut out[..oh * ow * c];
    let b = &b[..c];
    for oy in 0..oh {
        for (ox, o) in out[oy * ow * c..][..ow * c].chunks_exact_mut(c).enumerate() {
            o.copy_from_slice(b);
            for ky in 0..k {
                let iy = (oy * s + ky).wrapping_sub(pt);
                if iy >= ih {
                    continue;
                }
                let xrow = &x[iy * iw * c..][..iw * c];
                for kx in 0..k {
                    let ix = (ox * s + kx).wrapping_sub(pl);
                    if ix >= iw {
                        continue;
                    }
                    let xp = &xrow[ix * c..][..c];
                    let wp = &w[(ky * k + kx) * c..][..c];
                    for ((ov, xv), wv) in o.iter_mut().zip(xp).zip(wp) {
                        *ov += xv * wv;
                    }
                }
            }
            if A != 0 {
                for ov in o.iter_mut() {
                    *ov = act::<A>(*ov);
                }
            }
        }
    }
}
