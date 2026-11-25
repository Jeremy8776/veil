//! A 3D LUT pre-computed for NV12, so a picture costs one interpolated table read per pixel.
//!
//! Purpose: `.cube` LUTs map RGB, but the camera picture is NV12 (BT.601, limited range, as
//! `veil-app`'s preview conversion reads it). [`YuvLut::from_cube`] samples the LUT once per
//! point of a 33x33x33 Y,U,V grid, then widens the Y axis to all 256 values. [`YuvLut::apply`]
//! reads that table bilinearly across U,V, whose weights are shared by each 2x2 block: four
//! reads per luma pixel, and four per block for chroma (from the block's mean luma).
//!
//! Invariants:
//! - The table is always 33 x 33 x 256 entries (about 1.7 MB), whatever the `.cube` size.
//! - U,V grid point `i` sits at byte value `8 * i`, so a byte's cell and weight are a shift
//!   and a mask, and the identity is exact. Values are stored times 16 for sub-level precision.
//! - Grid points whose Y,U,V is outside the RGB cube are clipped to it first, as the preview
//!   and any RGB pipeline would, so a look such as black and white is complete even on noisy
//!   out-of-range camera pixels. An identity LUT can therefore move such pixels, but not what
//!   is shown, since showing them clips the same way.
//! - [`YuvLut::mixed`] blends the table with the identity. Interpolation is linear, so reading
//!   the blended table equals blending the full result with the original picture.
//! - A malformed picture (odd or zero size, `stride < width`, short buffer) is left untouched.

use super::cube::Cube;

/// U,V grid points per axis; also the Y points sampled before Y is widened.
pub const NODES: usize = 33;
/// Entries per U,V column: one per luma value.
const LEVELS: usize = 256;
const FIX: f32 = 16.0;
const TOP: u16 = 256 * 16;

/// A LUT as a U,V-indexed set of 256-entry luma columns: luma out and chroma out per entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YuvLut {
    luma: Vec<u16>,
    chroma: Vec<[u16; 2]>,
}

fn column(u: usize, v: usize) -> usize {
    (v * NODES + u) * LEVELS
}

/// Byte value of grid point `i`: 0, 8, ... 256.
fn node(i: usize) -> f32 {
    (i * 8) as f32
}

fn fixed(x: f32) -> u16 {
    (x * FIX).round().clamp(0.0, f32::from(TOP)) as u16
}

pub(super) fn yuv_to_rgb(y: f32, u: f32, v: f32) -> [f32; 3] {
    let l = (y - 16.0) / 219.0;
    let cb = (u - 128.0) / 224.0;
    let cr = (v - 128.0) / 224.0;
    [
        l + 1.402 * cr,
        l - 0.344_136 * cb - 0.714_136 * cr,
        l + 1.772 * cb,
    ]
}

fn rgb_to_yuv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = 0.299 * r + 0.587 * g + 0.114 * b;
    [
        16.0 + 219.0 * l,
        128.0 + 224.0 * (b - l) / 1.772,
        128.0 + 224.0 * (r - l) / 1.402,
    ]
}

impl YuvLut {
    /// Builds the table from `f(y, u, v) -> [y, u, v]`, sampled at the 33-point grid and
    /// widened linearly along Y.
    fn build(f: impl Fn(f32, f32, f32) -> [f32; 3]) -> Self {
        let n = NODES * NODES * LEVELS;
        let mut luma = vec![0; n];
        let mut chroma = vec![[0; 2]; n];
        let mut grid = [[0f32; 3]; NODES];
        for v in 0..NODES {
            for u in 0..NODES {
                for (y, g) in grid.iter_mut().enumerate() {
                    *g = f(node(y), node(u), node(v));
                }
                let col = column(u, v);
                for level in 0..LEVELS {
                    let (i, t) = (level >> 3, (level & 7) as f32 / 8.0);
                    let at = |c: usize| grid[i][c] + (grid[i + 1][c] - grid[i][c]) * t;
                    luma[col + level] = fixed(at(0));
                    chroma[col + level] = [fixed(at(1)), fixed(at(2))];
                }
            }
        }
        Self { luma, chroma }
    }

    /// Samples `cube` at every grid point. Inputs are mapped from 0..1 RGB into the cube's
    /// domain.
    pub fn from_cube(cube: &Cube) -> Self {
        Self::build(|y, u, v| {
            let inside = yuv_to_rgb(y, u, v).map(|c| c.clamp(0.0, 1.0));
            let input: [f32; 3] = std::array::from_fn(|c| {
                cube.domain_min[c] + inside[c] * (cube.domain_max[c] - cube.domain_min[c])
            });
            rgb_to_yuv(cube.sample(input).map(|c| c.clamp(0.0, 1.0)))
        })
    }

    /// A table that changes nothing: the 0% end of [`Self::mixed`].
    pub fn identity() -> Self {
        Self::build(|y, u, v| [y, u, v])
    }

    /// This table blended with the identity: `strength` 0 is the identity, 100 is `self`.
    pub fn mixed(&self, strength: u8) -> Self {
        let s = i32::from(strength.min(crate::effects::FULL));
        let mix = |full: u16, id: u16| -> u16 {
            let (f, o) = (i32::from(full), i32::from(id));
            (o + ((f - o) * s + 50).div_euclid(100)).clamp(0, i32::from(TOP)) as u16
        };
        let mut out = self.clone();
        for v in 0..NODES {
            for u in 0..NODES {
                let col = column(u, v);
                let (iu, iv) = (fixed(node(u)), fixed(node(v)));
                for level in 0..LEVELS {
                    let i = col + level;
                    out.luma[i] = mix(self.luma[i], fixed(level as f32));
                    out.chroma[i] = [mix(self.chroma[i][0], iu), mix(self.chroma[i][1], iv)];
                }
            }
        }
        out
    }

    /// Applies the table in place to an NV12 picture of `width` x `height`, row pitch `stride`.
    pub fn apply(&self, data: &mut [u8], width: usize, height: usize, stride: usize) {
        if !crate::blur::fits(data.len(), width, height, stride) {
            return;
        }
        let (luma, chroma) = data.split_at_mut(stride * height);
        for (c, uv_row) in chroma.chunks_mut(stride).take(height / 2).enumerate() {
            let (top, rest) = luma[2 * c * stride..].split_at_mut(stride);
            let bottom = &mut rest[..width];
            let top = &mut top[..width];
            for (bx, uv) in uv_row[..width]
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .enumerate()
            {
                let x = 2 * bx;
                let Some(block) = Block::new(self, uv[0], uv[1]) else {
                    continue;
                };
                let sum = u32::from(top[x])
                    + u32::from(top[x + 1])
                    + u32::from(bottom[x])
                    + u32::from(bottom[x + 1]);
                let [nu, nv] = block.chroma(((sum + 2) / 4) as u8);
                uv[0] = nu;
                uv[1] = nv;
                for row in [&mut *top, &mut *bottom] {
                    for p in &mut row[x..x + 2] {
                        *p = block.luma(*p);
                    }
                }
            }
        }
    }
}

/// The four U,V columns around one chroma sample and their weights, in 64ths.
struct Block<'a> {
    luma: [&'a [u16; LEVELS]; 4],
    chroma: [&'a [[u16; 2]; LEVELS]; 4],
    weight: [u32; 4],
}

impl<'a> Block<'a> {
    fn new(lut: &'a YuvLut, u: u8, v: u8) -> Option<Self> {
        let (ui, fu) = (usize::from(u >> 3), u32::from(u & 7));
        let (vi, fv) = (usize::from(v >> 3), u32::from(v & 7));
        let cols = [
            column(ui, vi),
            column(ui + 1, vi),
            column(ui, vi + 1),
            column(ui + 1, vi + 1),
        ];
        let luma = |c: usize| lut.luma.get(c..c + LEVELS)?.try_into().ok();
        let chroma = |c: usize| lut.chroma.get(c..c + LEVELS)?.try_into().ok();
        Some(Self {
            luma: [
                luma(cols[0])?,
                luma(cols[1])?,
                luma(cols[2])?,
                luma(cols[3])?,
            ],
            chroma: [
                chroma(cols[0])?,
                chroma(cols[1])?,
                chroma(cols[2])?,
                chroma(cols[3])?,
            ],
            weight: [(8 - fu) * (8 - fv), fu * (8 - fv), (8 - fu) * fv, fu * fv],
        })
    }

    fn luma(&self, y: u8) -> u8 {
        let y = usize::from(y);
        let sum: u32 = (0..4)
            .map(|k| self.weight[k] * u32::from(self.luma[k][y]))
            .sum();
        to_byte(sum)
    }

    fn chroma(&self, y: u8) -> [u8; 2] {
        let y = usize::from(y);
        let mut sum = [0u32; 2];
        for k in 0..4 {
            let [cu, cv] = self.chroma[k][y];
            sum[0] += self.weight[k] * u32::from(cu);
            sum[1] += self.weight[k] * u32::from(cv);
        }
        sum.map(to_byte)
    }
}

/// Sums are scaled by 64 (U,V weights) * 16 (stored precision).
const SHIFT: u32 = 6 + 4;

fn to_byte(sum: u32) -> u8 {
    ((sum + (1 << (SHIFT - 1))) >> SHIFT).min(255) as u8
}
