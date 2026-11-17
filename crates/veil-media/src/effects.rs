//! Sender-side picture filters, applied to a captured NV12 picture before it is encoded.
//!
//! Purpose: [`apply_filter`] changes tone and colour with three 256-entry lookup tables, one
//! each for luma, U and V, so the cost per picture is one table read per byte.
//!
//! Invariants:
//! - [`Filter::None`] returns at once and leaves the picture untouched.
//! - Input is tightly packed NV12 as in [`crate::capture`]: `stride * height` luma bytes, then
//!   `height / 2` rows of interleaved U,V, each `stride` bytes with `width` of them used.
//! - Safe on short or odd buffers: rows past the end are skipped, never indexed.

/// A look applied to the camera picture. The interface owns the labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    None,
    Warm,
    Cool,
    BlackWhite,
    Bright,
}

impl Filter {
    /// Every filter, in picker order.
    pub const ALL: [Filter; 5] = [
        Filter::None,
        Filter::Warm,
        Filter::Cool,
        Filter::BlackWhite,
        Filter::Bright,
    ];
}

/// How far warm and cool move U and V from neutral.
const TINT: u8 = 12;

fn lut(f: impl Fn(u8) -> u8) -> [u8; 256] {
    std::array::from_fn(|i| f(i as u8))
}

/// Applies `filter` in place to an NV12 picture of `width` x `height` with row pitch `stride`.
pub fn apply_filter(data: &mut [u8], width: usize, height: usize, stride: usize, filter: Filter) {
    if filter == Filter::None || stride == 0 {
        return;
    }
    let ident = |x: u8| x;
    let (y_lut, u_lut, v_lut) = match filter {
        Filter::None => return,
        Filter::Warm => (
            lut(ident),
            lut(|u| u.saturating_sub(TINT)),
            lut(|v| v.saturating_add(TINT)),
        ),
        Filter::Cool => (
            lut(ident),
            lut(|u| u.saturating_add(TINT)),
            lut(|v| v.saturating_sub(TINT)),
        ),
        Filter::BlackWhite => (lut(ident), lut(|_| 128), lut(|_| 128)),
        Filter::Bright => (lut(|y| y + (255 - y) / 4), lut(ident), lut(ident)),
    };
    let luma_len = stride.saturating_mul(height);
    let (luma, chroma) = match data.split_at_mut_checked(luma_len) {
        Some(parts) => parts,
        None => (data, &mut [][..]),
    };
    if filter == Filter::Bright {
        for row in luma.chunks_mut(stride) {
            for y in row.iter_mut().take(width) {
                *y = y_lut[usize::from(*y)];
            }
        }
    } else {
        for row in chroma.chunks_mut(stride).take(height / 2) {
            for uv in row.as_chunks_mut::<2>().0.iter_mut().take(width / 2) {
                uv[0] = u_lut[usize::from(uv[0])];
                uv[1] = v_lut[usize::from(uv[1])];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4x2 picture, stride 6 (two bytes of padding): 12 luma bytes then 6 chroma bytes.
    fn picture(y: u8, u: u8, v: u8) -> Vec<u8> {
        let mut d = vec![y; 12];
        d.extend([u, v, u, v, 0, 0]);
        d
    }

    fn mean_luma(d: &[u8]) -> u32 {
        d[..12].iter().map(|&b| u32::from(b)).sum::<u32>() / 12
    }

    #[test]
    fn none_is_unchanged() {
        let mut d = picture(90, 40, 200);
        let before = d.clone();
        apply_filter(&mut d, 4, 2, 6, Filter::None);
        assert_eq!(d, before);
    }

    #[test]
    fn black_white_neutralises_chroma_only() {
        let mut d = picture(90, 40, 200);
        apply_filter(&mut d, 4, 2, 6, Filter::BlackWhite);
        assert_eq!(&d[..12], &[90; 12]);
        assert_eq!(&d[12..16], &[128; 4]);
        assert_eq!(&d[16..], &[0, 0], "padding untouched");
    }

    #[test]
    fn tight_stride_works() {
        let mut d = vec![50, 60, 70, 80, 50, 60, 70, 80, 1, 2, 3, 4];
        apply_filter(&mut d, 4, 2, 4, Filter::BlackWhite);
        assert_eq!(&d[8..], &[128; 4]);
        assert_eq!(&d[..4], &[50, 60, 70, 80]);
    }

    #[test]
    fn bright_raises_luma() {
        let mut d = picture(100, 128, 128);
        apply_filter(&mut d, 4, 2, 6, Filter::Bright);
        assert!(mean_luma(&d) > 100);
        assert_eq!(&d[12..16], &[128; 4]);
    }

    #[test]
    fn warm_and_cool_move_opposite() {
        let (mut w, mut c) = (picture(90, 128, 128), picture(90, 128, 128));
        apply_filter(&mut w, 4, 2, 6, Filter::Warm);
        apply_filter(&mut c, 4, 2, 6, Filter::Cool);
        assert!(w[12] < 128 && w[13] > 128);
        assert!(c[12] > 128 && c[13] < 128);
    }

    #[test]
    fn extremes_saturate() {
        for f in Filter::ALL {
            for v in [0, 255] {
                let mut d = picture(v, v, v);
                apply_filter(&mut d, 4, 2, 6, f);
            }
        }
        let mut d = picture(255, 255, 0);
        apply_filter(&mut d, 4, 2, 6, Filter::Bright);
        assert_eq!(d[0], 255);
        let mut d = picture(0, 0, 255);
        apply_filter(&mut d, 4, 2, 6, Filter::Warm);
        assert_eq!((d[12], d[13]), (0, 255));
    }

    #[test]
    fn short_buffers_do_not_panic() {
        for len in 0..20 {
            for f in Filter::ALL {
                apply_filter(&mut vec![7; len], 4, 2, 6, f);
            }
        }
    }
}
