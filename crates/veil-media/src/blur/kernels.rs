//! Small row kernels for Background blur: shrink, box blur, bilinear steps and the blend.
//!
//! Purpose: the loops [`super::grid`] runs per picture, kept apart so each file stays small.
//!
//! Invariants:
//! - Callers pass slices of the right length; these loops index within them and never allocate.
//! - Box blurs work on 16-bit values, so a plane weighted by a mask (up to 255 x 255) fits.
//! - Safe code and plain loops only: no intrinsics, no CPU detection.

// lean: the row loops use chunks with sizes fixed by the grid or a const generic; as_chunks
// would need array types for each. Revisit if the grid size ever becomes a const-generic array.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::grid::Axis;

pub(super) type Ranges = [(usize, usize)];

pub(super) fn div25(s: u16) -> u8 {
    (((u32::from(s) + 12) * 2622) >> 16) as u8
}

fn div5(s: u16) -> u8 {
    (((u32::from(s) + 2) * 13108) >> 16) as u8
}

/// Area-average shrink of a plane with `C` interleaved channels to `dw` pixels per row.
/// `src` rows are `pitch` bytes apart, with `sw` pixels used.
pub(super) fn box_down<const C: usize>(
    src: &[u8],
    sw: usize,
    dst: &mut [u8],
    dw: usize,
    (xb, yb): (&Ranges, &Ranges),
    (cols, pitch): (&mut [u32], usize),
) {
    let cols = &mut cols[..sw * C];
    for (j, drow) in dst.chunks_exact_mut(dw * C).enumerate() {
        cols.fill(0);
        let (y0, y1) = yb[j];
        for y in y0..y1 {
            for (c, &p) in cols.iter_mut().zip(&src[y * pitch..][..sw * C]) {
                *c += u32::from(p);
            }
        }
        for (i, &(x0, x1)) in xb.iter().enumerate() {
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            for c in 0..C {
                let s: u32 = (x0..x1).map(|x| cols[x * C + c]).sum();
                drow[i * C + c] = ((s + n / 2) / n) as u8;
            }
        }
    }
}

/// Rounded `sum / n` for box sums, with `recip = ceil(2^32 / n)` (see [`recip`]).
fn scale(sum: u32, recip: u64) -> u16 {
    ((u64::from(sum) * recip + (1 << 31)) >> 32) as u16
}

/// `ceil(2^32 / (2r + 1))`, the multiplier [`scale`] uses for a box of radius `r`.
pub(super) fn recip(r: usize) -> u64 {
    let n = 2 * r as u64 + 1;
    (1u64 << 32).div_ceil(n)
}

/// Sliding-window box blur along rows, edge-clamped, 16-bit values. `pad` is scratch.
pub(super) fn blur_h<const C: usize>(
    src: &[u16],
    dst: &mut [u16],
    w: usize,
    r: usize,
    recip: u64,
    pad: &mut [u16],
) {
    let pad = &mut pad[..(w + 2 * r + 1) * C];
    for (s, d) in src.chunks_exact(w * C).zip(dst.chunks_exact_mut(w * C)) {
        for i in 0..r {
            pad[i * C..][..C].copy_from_slice(&s[..C]);
        }
        pad[r * C..][..w * C].copy_from_slice(s);
        for i in 0..=r {
            pad[(r + w + i) * C..][..C].copy_from_slice(&s[(w - 1) * C..]);
        }
        let mut sum = [0u32; C];
        for px in pad[..(2 * r + 1) * C].chunks_exact(C) {
            for c in 0..C {
                sum[c] += u32::from(px[c]);
            }
        }
        for (x, o) in d.chunks_exact_mut(C).enumerate() {
            let (out, inn) = (&pad[x * C..][..C], &pad[(x + 2 * r + 1) * C..][..C]);
            for c in 0..C {
                o[c] = scale(sum[c], recip);
                sum[c] = sum[c] + u32::from(inn[c]) - u32::from(out[c]);
            }
        }
    }
}

/// Sliding-window box blur down columns, edge-clamped, one whole row at a time, 16-bit values.
pub(super) fn blur_v(
    src: &[u16],
    dst: &mut [u16],
    rl: usize,
    h: usize,
    r: usize,
    recip: u64,
    acc: &mut [u32],
) {
    let acc = &mut acc[..rl];
    acc.fill(0);
    for k in 0..=2 * r {
        let y = k.saturating_sub(r).min(h - 1);
        for (a, &p) in acc.iter_mut().zip(&src[y * rl..][..rl]) {
            *a += u32::from(p);
        }
    }
    for y in 0..h {
        for (o, &a) in dst[y * rl..][..rl].iter_mut().zip(acc.iter()) {
            *o = scale(a, recip);
        }
        let add = &src[(y + r + 1).min(h - 1) * rl..][..rl];
        let sub = &src[y.saturating_sub(r) * rl..][..rl];
        for ((a, &p), &q) in acc.iter_mut().zip(add).zip(sub) {
            *a = a.wrapping_add(u32::from(p)).wrapping_sub(u32::from(q));
        }
    }
}

/// Vertical bilinear step between two small rows, 8-bit weight of `b`.
pub(super) fn lerp(a: &[u8], b: &[u8], w: u16, out: &mut [u8]) {
    for ((o, &p), &q) in out.iter_mut().zip(a).zip(b) {
        *o = ((u16::from(p) * (256 - w) + u16::from(q) * w + 128) >> 8) as u8;
    }
}

/// Horizontal bilinear step of one small row to full width (`c` interleaved channels).
pub(super) fn expand(src: &[u8], dst: &mut [u8], c: usize, fast: bool, ax: &Axis, pad: &mut [u8]) {
    if fast {
        if c == 1 {
            expand5::<1>(src, dst, pad);
        } else {
            expand5::<2>(src, dst, pad);
        }
        return;
    }
    for (x, d) in dst.chunks_exact_mut(c).enumerate() {
        let (a, b, w) = (ax.i0[x], ax.i1[x], u32::from(ax.w[x]));
        for (k, o) in d.iter_mut().enumerate() {
            *o = ((u32::from(src[a * c + k]) * (256 - w) + u32::from(src[b * c + k]) * w + 128)
                >> 8) as u8;
        }
    }
}

/// Exact x5 bilinear expand (fifths weights) for the 1280-wide fast path.
fn expand5<const C: usize>(src: &[u8], dst: &mut [u8], pad: &mut [u8]) {
    let n = src.len() / C;
    let pad = &mut pad[..(n + 2) * C];
    pad[..C].copy_from_slice(&src[..C]);
    pad[C..(n + 1) * C].copy_from_slice(src);
    pad[(n + 1) * C..].copy_from_slice(&src[(n - 1) * C..]);
    for (i, o) in dst[..n * 5 * C].chunks_exact_mut(5 * C).enumerate() {
        let p = &pad[i * C..][..3 * C];
        for c in 0..C {
            let (l, m, r) = (
                u16::from(p[c]),
                u16::from(p[C + c]),
                u16::from(p[2 * C + c]),
            );
            o[c] = div5(2 * l + 3 * m);
            o[C + c] = div5(l + 4 * m);
            o[2 * C + c] = m as u8;
            o[3 * C + c] = div5(4 * m + r);
            o[4 * C + c] = div5(3 * m + 2 * r);
        }
    }
}

/// `px = (m * px + (255 - m) * blur) / 255`, rounded.
pub(super) fn blend(px: &mut [u8], m: &[u8], b: &[u8]) {
    for ((o, &m), &b) in px.iter_mut().zip(m).zip(b) {
        let m = u16::from(m);
        let x = m * u16::from(*o) + (255 - m) * u16::from(b) + 128;
        *o = ((x + (x >> 8)) >> 8) as u8;
    }
}
