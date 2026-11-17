//! The picture-sized half of Background blur: shrink, model input, small-grid blur, composite.
//!
//! Purpose: [`Pipeline`] makes one 256x144 copy of the NV12 picture (luma 256x144, chroma
//! 128x72), builds the model input and the blurred small planes from it, then composites
//! `m * original + (1 - m) * blurred` back onto the full picture in place.
//!
//! The blur is weighted by the background: each pixel's share is `1 - m`, so the person's
//! colours do not smear into the blurred background beside them (no halo). The mask's edge is
//! made steeper with [`EDGE`] before use, so the outline is tight rather than a soft ramp.
//!
//! Invariants:
//! - A pipeline is built for one even `width` x `height`. Row pitch (`stride`) is given per call.
//! - Callers check the buffer first (see `fits` in the parent module); nothing here is
//!   checked again, and rows are read and written only up to `width` bytes.
//! - Exactly 1280x720 takes a fast path (x5 steps). Every other size takes the general path,
//!   which also stretches pictures smaller than the grid (nearest on the way in).
//! - All scratch is allocated in [`Pipeline::new`]; the per-picture methods allocate nothing.
//! - Safe code and plain loops only: no intrinsics, no CPU detection.

// lean: the row loops use chunks with sizes fixed by the grid or a const generic; as_chunks
// would need array types for each. Revisit if the grid size ever becomes a const-generic array.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::kernels::{blend, blur_h, blur_v, box_down, div25, expand, lerp, recip};

/// Model and small-grid width.
pub const MW: usize = 256;
/// Model and small-grid height.
pub const MH: usize = 144;
const CW: usize = MW / 2;
const CH: usize = MH / 2;
const PLANE: usize = MW * MH;
const CPLANE: usize = CW * CH * 2;

/// Mask values below `EDGE.0` count as background and above `EDGE.1` as person, with a smooth
/// step between. The model's mask is soft and the 5x upscale softens it further; this tightens
/// the outline to about two picture pixels at 1280x720.
pub const EDGE: (f32, f32) = (64.0, 192.0);

/// Background weight (out of 255) below which a blurred pixel has too little background around
/// it to be worth dividing; it keeps its plain value. Only pixels well inside the person are.
const MIN_WEIGHT: u16 = 4;

fn edge_lut() -> [u8; 256] {
    std::array::from_fn(|i| {
        let t = ((i as f32 - EDGE.0) / (EDGE.1 - EDGE.0)).clamp(0.0, 1.0);
        (t * t * (3.0 - 2.0 * t) * 255.0).round() as u8
    })
}

/// Box blur size on the small grid.
#[derive(Clone, Copy)]
pub struct Strength {
    pub radius: usize,
    pub passes: usize,
}

/// Bilinear source indices and the 8-bit weight of `i1`, per destination position.
pub(super) struct Axis {
    pub(super) i0: Vec<usize>,
    pub(super) i1: Vec<usize>,
    pub(super) w: Vec<u16>,
}

fn axis(ns: usize, nd: usize) -> Axis {
    let (mut i0, mut i1, mut w) = (vec![], vec![], vec![]);
    for x in 0..nd {
        let s = ((x as f64 + 0.5) * ns as f64 / nd as f64 - 0.5).max(0.0);
        let a = (s.floor() as usize).min(ns - 1);
        i0.push(a);
        i1.push((a + 1).min(ns - 1));
        w.push(((s - a as f64) * 256.0).round() as u16);
    }
    Axis { i0, i1, w }
}

/// Source range `[lo, hi)` for each of `nd` destination pixels; never empty.
fn bounds(ns: usize, nd: usize) -> Vec<(usize, usize)> {
    (0..nd)
        .map(|i| {
            let lo = i * ns / nd;
            (lo, ((i + 1) * ns / nd).max(lo + 1).min(ns))
        })
        .collect()
}

pub struct Pipeline {
    pub w: usize,
    pub h: usize,
    fast: bool,
    /// Model input, planar CHW RGB in [0, 1]. The engine swaps this with the worker's slot.
    pub input: Vec<f32>,
    sy: Vec<u8>,
    suv: Vec<u8>,
    /// Blurred small planes, the result of [`Pipeline::blur`].
    ya: Vec<u8>,
    ua: Vec<u8>,
    /// Background-weighted planes and their weights, blurred in place (`*b` is scratch).
    py: Vec<u16>,
    wy: Vec<u16>,
    pc: Vec<u16>,
    wc: Vec<u16>,
    tmp_y: Vec<u16>,
    tmp_c: Vec<u16>,
    pad16: Vec<u16>,
    lut: [u8; 256],
    cols16: Vec<u16>,
    cols32: Vec<u32>,
    acc: Vec<u32>,
    pad: Vec<u8>,
    vt: Vec<u8>,
    m_row: Vec<u8>,
    b_row: Vec<u8>,
    mc_row: Vec<u8>,
    full: Vec<bool>,
    mx: Axis,
    my: Axis,
    cx: Axis,
    cy: Axis,
    bx: Vec<(usize, usize)>,
    by: Vec<(usize, usize)>,
    bcx: Vec<(usize, usize)>,
    bcy: Vec<(usize, usize)>,
}

impl Pipeline {
    /// `w` and `h` must be even and non-zero.
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            fast: w == 1280 && h == 720,
            input: vec![0.0; 3 * PLANE],
            sy: vec![0; PLANE],
            suv: vec![0; CW * CH * 2],
            ya: vec![0; PLANE],
            ua: vec![0; CPLANE],
            py: vec![0; PLANE],
            wy: vec![0; PLANE],
            pc: vec![0; CPLANE],
            wc: vec![0; CPLANE],
            tmp_y: vec![0; PLANE],
            tmp_c: vec![0; CPLANE],
            pad16: vec![0; 1024],
            lut: edge_lut(),
            cols16: vec![0; w * 2],
            cols32: vec![0; w * 2],
            acc: vec![0; MW],
            pad: vec![0; 512],
            vt: vec![0; MW],
            m_row: vec![0; w],
            b_row: vec![0; w],
            mc_row: vec![0; w],
            full: vec![false; MH],
            mx: axis(MW, w),
            my: axis(MH, h),
            cx: axis(CW, w / 2),
            cy: axis(CH, h / 2),
            bx: bounds(w, MW),
            by: bounds(h, MH),
            bcx: bounds(w / 2, CW),
            bcy: bounds(h / 2, CH),
        }
    }

    #[cfg(test)]
    pub fn force_general(&mut self) {
        self.fast = false;
    }

    /// Shrinks the picture to the small grid. Reads only.
    pub fn downscale(&mut self, data: &[u8], stride: usize) {
        let (w, h) = (self.w, self.h);
        let (py, puv) = data.split_at(stride * h);
        if self.fast {
            for (j, o) in self.sy.chunks_exact_mut(MW).enumerate() {
                let cols = &mut self.cols16[..w];
                cols.fill(0);
                for r in 0..5 {
                    for (c, &p) in cols.iter_mut().zip(&py[(j * 5 + r) * stride..][..w]) {
                        *c += u16::from(p);
                    }
                }
                for (d, c) in o.iter_mut().zip(cols.chunks_exact(5)) {
                    *d = div25(c[0] + c[1] + c[2] + c[3] + c[4]);
                }
            }
            for (j, o) in self.suv.chunks_exact_mut(CW * 2).enumerate() {
                let cols = &mut self.cols16[..w];
                cols.fill(0);
                for r in 0..5 {
                    for (c, &p) in cols.iter_mut().zip(&puv[(j * 5 + r) * stride..][..w]) {
                        *c += u16::from(p);
                    }
                }
                for (d, c) in o.chunks_exact_mut(2).zip(cols.chunks_exact(10)) {
                    d[0] = div25(c[0] + c[2] + c[4] + c[6] + c[8]);
                    d[1] = div25(c[1] + c[3] + c[5] + c[7] + c[9]);
                }
            }
        } else {
            let bufs = (&mut self.cols32[..], stride);
            box_down::<1>(py, w, &mut self.sy, MW, (&self.bx, &self.by), bufs);
            let bufs = (&mut self.cols32[..], stride);
            box_down::<2>(puv, w / 2, &mut self.suv, CW, (&self.bcx, &self.bcy), bufs);
        }
    }

    /// BT.709 limited range to full-range RGB in [0, 1], planar CHW, from the small grid.
    pub fn make_input(&mut self) {
        const KY: f32 = 1.164383 / 255.0;
        let (rv, gu, gv, bu) = (
            1.792741 / 255.0,
            0.213249 / 255.0,
            0.532909 / 255.0,
            2.112402 / 255.0,
        );
        let (r, rest) = self.input.split_at_mut(PLANE);
        let (g, b) = rest.split_at_mut(PLANE);
        for y in 0..MH {
            let yr = &self.sy[y * MW..][..MW];
            let uvr = &self.suv[(y / 2) * MW..][..MW];
            let (rr, gr, br) = (
                &mut r[y * MW..][..MW],
                &mut g[y * MW..][..MW],
                &mut b[y * MW..][..MW],
            );
            let px = yr.chunks_exact(2).zip(uvr.chunks_exact(2));
            let out = rr
                .chunks_exact_mut(2)
                .zip(gr.chunks_exact_mut(2))
                .zip(br.chunks_exact_mut(2));
            for ((yp, uv), ((rp, gp), bp)) in px.zip(out) {
                let (u, v) = (f32::from(uv[0]) - 128.0, f32::from(uv[1]) - 128.0);
                let (dr, dg, db) = (rv * v, -gu * u - gv * v, bu * u);
                for i in 0..2 {
                    let yy = (f32::from(yp[i]) - 16.0) * KY;
                    rp[i] = (yy + dr).clamp(0.0, 1.0);
                    gp[i] = (yy + dg).clamp(0.0, 1.0);
                    bp[i] = (yy + db).clamp(0.0, 1.0);
                }
            }
        }
    }

    /// Background-weighted box blur of the small planes, `mask` 256x144 (255 = person).
    /// Result in `ya`, `ua`.
    pub fn blur(&mut self, s: Strength, mask: &[u8]) {
        let (r, k) = (s.radius, recip(s.radius));
        for (((p, w), &v), &m) in self.py.iter_mut().zip(&mut self.wy).zip(&self.sy).zip(mask) {
            *w = 255 - u16::from(self.lut[usize::from(m)]);
            *p = *w * u16::from(v);
        }
        for cy in 0..CH {
            for cx in 0..CW {
                let i = 2 * cy * MW + 2 * cx;
                let q = &self.wy;
                let w = (q[i] + q[i + 1] + q[i + MW] + q[i + MW + 1] + 2) / 4;
                let j = (cy * CW + cx) * 2;
                for c in 0..2 {
                    self.wc[j + c] = w;
                    self.pc[j + c] = w * u16::from(self.suv[j + c]);
                }
            }
        }
        for _ in 0..s.passes {
            for plane in [&mut self.py, &mut self.wy] {
                blur_h::<1>(plane, &mut self.tmp_y, MW, r, k, &mut self.pad16);
                std::mem::swap(plane, &mut self.tmp_y);
            }
            for plane in [&mut self.pc, &mut self.wc] {
                blur_h::<2>(plane, &mut self.tmp_c, CW, r, k, &mut self.pad16);
                std::mem::swap(plane, &mut self.tmp_c);
            }
        }
        for _ in 0..s.passes {
            for plane in [&mut self.py, &mut self.wy] {
                blur_v(plane, &mut self.tmp_y, MW, MH, r, k, &mut self.acc);
                std::mem::swap(plane, &mut self.tmp_y);
            }
            for plane in [&mut self.pc, &mut self.wc] {
                blur_v(plane, &mut self.tmp_c, CW * 2, CH, r, k, &mut self.acc);
                std::mem::swap(plane, &mut self.tmp_c);
            }
        }
        unweight(&mut self.ya, &self.py, &self.wy, &self.sy);
        unweight(&mut self.ua, &self.pc, &self.wc, &self.suv);
    }

    /// `out = m * orig + (1 - m) * blur_up`, in place. `mask` is 256x144, 255 = person.
    pub fn composite(&mut self, data: &mut [u8], stride: usize, mask: &[u8]) {
        let (w, h) = (self.w, self.h);
        for (f, row) in self.full.iter_mut().zip(mask.chunks_exact(MW)) {
            *f = row.iter().all(|&v| v == 255);
        }
        let (py, puv) = data.split_at_mut(stride * h);
        for y in 0..h {
            let (a, b, wy) = (self.my.i0[y], self.my.i1[y], self.my.w[y]);
            if self.full[a] && self.full[b] {
                continue;
            }
            lerp(
                &mask[a * MW..][..MW],
                &mask[b * MW..][..MW],
                wy,
                &mut self.vt,
            );
            expand(
                &self.vt,
                &mut self.m_row,
                1,
                self.fast,
                &self.mx,
                &mut self.pad,
            );
            for m in self.m_row.iter_mut() {
                *m = self.lut[usize::from(*m)];
            }
            lerp(
                &self.ya[a * MW..][..MW],
                &self.ya[b * MW..][..MW],
                wy,
                &mut self.vt,
            );
            expand(
                &self.vt,
                &mut self.b_row,
                1,
                self.fast,
                &self.mx,
                &mut self.pad,
            );
            blend(&mut py[y * stride..][..w], &self.m_row, &self.b_row);
            if y & 1 == 0 {
                let c = y / 2;
                let (a, b, wy) = (self.cy.i0[c], self.cy.i1[c], self.cy.w[c]);
                lerp(
                    &self.ua[a * MW..][..MW],
                    &self.ua[b * MW..][..MW],
                    wy,
                    &mut self.vt,
                );
                expand(
                    &self.vt,
                    &mut self.b_row,
                    2,
                    self.fast,
                    &self.cx,
                    &mut self.pad,
                );
                // One mask value per U,V pair: the average of the two luma-res mask samples.
                for (d, m) in self
                    .mc_row
                    .chunks_exact_mut(2)
                    .zip(self.m_row.chunks_exact(2))
                {
                    let p = ((u16::from(m[0]) + u16::from(m[1]) + 1) >> 1) as u8;
                    d[0] = p;
                    d[1] = p;
                }
                blend(&mut puv[c * stride..][..w], &self.mc_row, &self.b_row);
            }
        }
    }
}

/// `out = weighted / weight`, rounded; `plain` where the weight is too small to divide by.
fn unweight(out: &mut [u8], weighted: &[u16], weight: &[u16], plain: &[u8]) {
    for (((o, &p), &w), &v) in out.iter_mut().zip(weighted).zip(weight).zip(plain) {
        *o = if w < MIN_WEIGHT {
            v
        } else {
            ((u32::from(p) + u32::from(w) / 2) / u32::from(w)).min(255) as u8
        };
    }
}
