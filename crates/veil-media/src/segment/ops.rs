//! Small network operators and the activation functions.
//!
//! Purpose: everything the network does besides the three convolution kinds in `kernels.rs`:
//! global mean, channel scaling, residual add, 2x bilinear resize and the final transposed
//! convolution.
//!
//! Invariants:
//! - Activations are NHWC (channel innermost) f32.
//! - Slices are cut to the exact length first, so short buffers panic early in tests, never
//!   read out of range (there is no `unsafe`).
//! - `exp` is a polynomial, not libm, so it vectorises on baseline SSE2 and gives the same
//!   result on every platform. Relative error is about 2e-7.

/// `exp(x)` by range reduction and a degree-6 polynomial, with `x` clamped to +-87.
#[inline(always)]
fn fexp(x: f32) -> f32 {
    let x = x.clamp(-87.0, 87.0);
    // 1.5 * 2^23: adding then subtracting it rounds to nearest without a rounding instruction.
    const MAGIC: f32 = 12_582_912.0;
    let n = (x * std::f32::consts::LOG2_E + MAGIC) - MAGIC;
    let r = (x - n * 0.693_145_75) - n * 1.428_606_8e-6;
    let p = 1.0
        + r * (1.0
            + r * (0.5
                + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0))))));
    p * f32::from_bits((((n as i32) + 127) as u32) << 23)
}

/// The logistic function.
#[inline(always)]
pub(super) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + fexp(-x))
}

/// Mean over pixels of each channel: `px` pixels of `c` channels into `out[..c]`.
pub(super) fn mean(x: &[f32], out: &mut [f32], px: usize, c: usize) {
    let out = &mut out[..c];
    out.fill(0.0);
    for p in x[..px * c].chunks_exact(c) {
        for (a, v) in out.iter_mut().zip(p) {
            *a += v;
        }
    }
    let inv = 1.0 / px as f32;
    for a in out.iter_mut() {
        *a *= inv;
    }
}

/// Scales every pixel of `x` (in place) by the per-channel vector `s`.
pub(super) fn scale_in_place(x: &mut [f32], s: &[f32], px: usize, c: usize) {
    for p in x[..px * c].chunks_exact_mut(c) {
        for (a, m) in p.iter_mut().zip(&s[..c]) {
            *a *= m;
        }
    }
}

/// Scales every pixel of `x` by the per-channel vector `s` into `out`.
pub(super) fn scale(x: &[f32], s: &[f32], out: &mut [f32], px: usize, c: usize) {
    let pairs = out[..px * c]
        .chunks_exact_mut(c)
        .zip(x[..px * c].chunks_exact(c));
    for (p, q) in pairs {
        for ((a, v), m) in p.iter_mut().zip(q).zip(&s[..c]) {
            *a = v * m;
        }
    }
}

/// `dst += src`, element-wise over `len` values.
pub(super) fn add_in_place(dst: &mut [f32], src: &[f32], len: usize) {
    for (d, s) in dst[..len].iter_mut().zip(&src[..len]) {
        *d += s;
    }
}

/// `out = a + b`, element-wise over `len` values.
pub(super) fn add(a: &[f32], b: &[f32], out: &mut [f32], len: usize) {
    for ((o, x), y) in out[..len].iter_mut().zip(&a[..len]).zip(&b[..len]) {
        *o = x + y;
    }
}

/// Bilinear x2 with half-pixel centres and edge clamp (TFLite `RESIZE_BILINEAR`, half-pixel).
pub(super) fn resize2x(x: &[f32], out: &mut [f32], ih: usize, iw: usize, c: usize) {
    let (oh, ow) = (ih * 2, iw * 2);
    let x = &x[..ih * iw * c];
    let out = &mut out[..oh * ow * c];
    for oy in 0..oh {
        let sy = ((oy as f32 + 0.5) * 0.5 - 0.5).max(0.0);
        let y0 = sy as usize;
        let fy = sy - y0 as f32;
        let y1 = (y0 + 1).min(ih - 1);
        let r0 = &x[y0 * iw * c..][..iw * c];
        let r1 = &x[y1 * iw * c..][..iw * c];
        for (ox, o) in out[oy * ow * c..][..ow * c].chunks_exact_mut(c).enumerate() {
            let sx = ((ox as f32 + 0.5) * 0.5 - 0.5).max(0.0);
            let x0 = sx as usize;
            let fx = sx - x0 as f32;
            let x1 = (x0 + 1).min(iw - 1);
            let (a, b) = (&r0[x0 * c..][..c], &r0[x1 * c..][..c]);
            let (cc, dd) = (&r1[x0 * c..][..c], &r1[x1 * c..][..c]);
            for i in 0..c {
                let top = a[i] + (b[i] - a[i]) * fx;
                let bot = cc[i] + (dd[i] - cc[i]) * fx;
                o[i] = top + (bot - top) * fy;
            }
        }
    }
}

/// 2x2 stride-2 transposed convolution, 16 channels to 1, into `logits` (row-major, `2 * iw`
/// wide). `w` is 4 taps of 16 weights, `bias` the single bias.
pub(super) fn tconv(x: &[f32], w: &[f32], bias: f32, logits: &mut [f32], px: usize, iw: usize) {
    let ow = iw * 2;
    for (p, xs) in x[..px * 16].as_chunks::<16>().0.iter().enumerate() {
        let (i, j) = (p / iw, p % iw);
        for t in 0..4 {
            let mut acc = [0.0f32; 4];
            let taps = xs
                .as_chunks::<4>()
                .0
                .iter()
                .zip(w[t * 16..][..16].as_chunks::<4>().0);
            for (xc, wc) in taps {
                for l in 0..4 {
                    acc[l] += xc[l] * wc[l];
                }
            }
            logits[(2 * i + t / 2) * ow + 2 * j + t % 2] =
                (acc[0] + acc[1]) + (acc[2] + acc[3]) + bias;
        }
    }
}
