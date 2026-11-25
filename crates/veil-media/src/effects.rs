//! Sender-side picture filters, applied to a captured NV12 picture before it is encoded.
//!
//! Purpose: [`apply_filter`] changes tone and colour with three 256-entry lookup tables, one
//! each for luma, U and V, so the cost per picture is one table read per byte. [`mirror`] flips
//! a picture left to right.
//!
//! Invariants:
//! - [`Filter::None`] and a strength of 0 return at once and leave the picture untouched.
//! - Strength (0 to [`FULL`] percent) is blended into the tables, so it costs nothing per
//!   pixel, and [`FULL`] gives exactly the full filter.
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

/// Full strength, in percent. Strength is how much of an effect is blended over the original
/// picture: 0 leaves it unchanged, [`FULL`] is the whole effect.
pub const FULL: u8 = 100;

/// `orig` moved towards `full` by `strength` percent (capped at [`FULL`]), rounded.
pub fn mix(orig: u8, full: u8, strength: u8) -> u8 {
    let s = i32::from(strength.min(FULL));
    let (o, f) = (i32::from(orig), i32::from(full));
    (o + ((f - o) * s + if f >= o { 50 } else { -50 }) / 100) as u8
}

/// How far warm and cool move U and V from neutral.
const TINT: u8 = 12;

/// Applies `filter` at `strength` percent in place to an NV12 picture of `width` x `height`
/// with row pitch `stride`.
pub fn apply_filter(
    data: &mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    filter: Filter,
    strength: u8,
) {
    if filter == Filter::None || stride == 0 || strength == 0 {
        return;
    }
    let lut = |f: &dyn Fn(u8) -> u8| -> [u8; 256] {
        std::array::from_fn(|i| mix(i as u8, f(i as u8), strength))
    };
    let ident = |x: u8| x;
    let (y_lut, u_lut, v_lut) = match filter {
        Filter::None => return,
        Filter::Warm => (
            lut(&ident),
            lut(&|u| u.saturating_sub(TINT)),
            lut(&|v| v.saturating_add(TINT)),
        ),
        Filter::Cool => (
            lut(&ident),
            lut(&|u| u.saturating_add(TINT)),
            lut(&|v| v.saturating_sub(TINT)),
        ),
        Filter::BlackWhite => (lut(&ident), lut(&|_| 128), lut(&|_| 128)),
        Filter::Bright => (lut(&|y| y + (255 - y) / 4), lut(&ident), lut(&ident)),
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

/// Flips a picture left to right. Row padding stays where it is, and each chroma pair keeps its
/// U,V order.
pub fn mirror(data: &mut [u8], width: usize, height: usize, stride: usize) {
    if stride == 0 || width > stride {
        return;
    }
    let luma_len = stride.saturating_mul(height);
    let (luma, chroma) = match data.split_at_mut_checked(luma_len) {
        Some(parts) => parts,
        None => (data, &mut [][..]),
    };
    for row in luma.chunks_exact_mut(stride) {
        row[..width].reverse();
    }
    for row in chroma.chunks_exact_mut(stride).take(height / 2) {
        let pairs = &mut row[..width & !1];
        pairs.reverse();
        for pair in pairs.as_chunks_mut::<2>().0 {
            pair.swap(0, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_flips_rows_and_keeps_padding_and_chroma_order() {
        // 4x2, stride 6: padding is 90 and 91 in luma, 92 and 93 in chroma.
        let mut d = vec![1, 2, 3, 4, 90, 91, 5, 6, 7, 8, 90, 91, 10, 20, 30, 40, 92, 93];
        mirror(&mut d, 4, 2, 6);
        assert_eq!(d, [4, 3, 2, 1, 90, 91, 8, 7, 6, 5, 90, 91, 30, 40, 10, 20, 92, 93]);
        mirror(&mut d, 4, 2, 6);
        assert_eq!(d[..4], [1, 2, 3, 4], "twice gives the picture back");
    }

    #[test]
    fn mirror_is_safe_on_short_or_bad_sizes() {
        let mut short = vec![1, 2, 3];
        mirror(&mut short, 4, 2, 6);
        mirror(&mut short, 4, 2, 0);
        mirror(&mut short, 8, 2, 6);
        assert_eq!(short, [1, 2, 3]);
    }

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
        apply_filter(&mut d, 4, 2, 6, Filter::None, FULL);
        assert_eq!(d, before);
    }

    #[test]
    fn black_white_neutralises_chroma_only() {
        let mut d = picture(90, 40, 200);
        apply_filter(&mut d, 4, 2, 6, Filter::BlackWhite, FULL);
        assert_eq!(&d[..12], &[90; 12]);
        assert_eq!(&d[12..16], &[128; 4]);
        assert_eq!(&d[16..], &[0, 0], "padding untouched");
    }

    #[test]
    fn tight_stride_works() {
        let mut d = vec![50, 60, 70, 80, 50, 60, 70, 80, 1, 2, 3, 4];
        apply_filter(&mut d, 4, 2, 4, Filter::BlackWhite, FULL);
        assert_eq!(&d[8..], &[128; 4]);
        assert_eq!(&d[..4], &[50, 60, 70, 80]);
    }

    #[test]
    fn bright_raises_luma() {
        let mut d = picture(100, 128, 128);
        apply_filter(&mut d, 4, 2, 6, Filter::Bright, FULL);
        assert!(mean_luma(&d) > 100);
        assert_eq!(&d[12..16], &[128; 4]);
    }

    #[test]
    fn warm_and_cool_move_opposite() {
        let (mut w, mut c) = (picture(90, 128, 128), picture(90, 128, 128));
        apply_filter(&mut w, 4, 2, 6, Filter::Warm, FULL);
        apply_filter(&mut c, 4, 2, 6, Filter::Cool, FULL);
        assert!(w[12] < 128 && w[13] > 128);
        assert!(c[12] > 128 && c[13] < 128);
    }

    #[test]
    fn extremes_saturate() {
        for f in Filter::ALL {
            for v in [0, 255] {
                let mut d = picture(v, v, v);
                apply_filter(&mut d, 4, 2, 6, f, FULL);
            }
        }
        let mut d = picture(255, 255, 0);
        apply_filter(&mut d, 4, 2, 6, Filter::Bright, FULL);
        assert_eq!(d[0], 255);
        let mut d = picture(0, 0, 255);
        apply_filter(&mut d, 4, 2, 6, Filter::Warm, FULL);
        assert_eq!((d[12], d[13]), (0, 255));
    }

    #[test]
    fn short_buffers_do_not_panic() {
        for len in 0..20 {
            for f in Filter::ALL {
                apply_filter(&mut vec![7; len], 4, 2, 6, f, FULL);
            }
        }
    }

    #[test]
    fn mix_ends_and_middle() {
        assert_eq!(mix(10, 200, 0), 10);
        assert_eq!(mix(10, 200, FULL), 200);
        assert_eq!(mix(10, 200, 50), 105);
        assert_eq!(mix(200, 10, 50), 105);
        assert_eq!(mix(0, 255, 255), 255, "over 100% is capped");
    }

    #[test]
    fn every_filter_at_zero_strength_is_unchanged() {
        for f in Filter::ALL {
            let mut d = picture(90, 40, 200);
            let before = d.clone();
            apply_filter(&mut d, 4, 2, 6, f, 0);
            assert_eq!(d, before, "{f:?}");
        }
    }

    #[test]
    fn half_strength_lands_between_original_and_full() {
        for f in Filter::ALL {
            let orig = picture(90, 40, 200);
            let (mut half, mut full) = (orig.clone(), orig.clone());
            apply_filter(&mut half, 4, 2, 6, f, 50);
            apply_filter(&mut full, 4, 2, 6, f, FULL);
            for i in 0..orig.len() {
                assert_eq!(half[i], mix(orig[i], full[i], 50), "{f:?} byte {i}");
            }
        }
    }
}
