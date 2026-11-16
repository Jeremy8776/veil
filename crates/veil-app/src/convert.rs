//! NV12 to RGBA for drawing a picture in the interface.
//!
//! Purpose: cameras and decoders give NV12; egui textures take RGBA. With `half`, each 2x2
//! block of luma is averaged and paired with its one chroma sample, which halves the size and
//! costs a quarter of the work, enough for a preview tile.
//!
//! Invariants:
//! - BT.709 limited range, what HD cameras and the H.264 encoders use.
//! - Never panics: a buffer too short for its stated size gives `None`.
//!
//! shortcut: converts on the CPU every frame. Do it in a wgpu shader when video tiles move to
//! custom paint callbacks.

/// A picture ready to upload: RGBA, 4 bytes a pixel, rows packed.
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// Converts a packed or strided NV12 picture. `None` if the buffer is too short or a size
/// is odd or zero.
pub fn nv12_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    half: bool,
) -> Option<Rgba> {
    let even = width.is_multiple_of(2) && height.is_multiple_of(2);
    if width == 0 || height == 0 || !even || stride < width {
        return None;
    }
    let chroma = stride.checked_mul(height)?;
    let needed = chroma.checked_add(stride.checked_mul(height / 2 - 1)?.checked_add(width)?)?;
    if data.len() < needed {
        return None;
    }
    let (out_w, out_h) = if half {
        (width / 2, height / 2)
    } else {
        (width, height)
    };
    let mut pixels = Vec::with_capacity(out_w * out_h * 4);
    for oy in 0..out_h {
        for ox in 0..out_w {
            let (cx, cy) = if half { (ox, oy) } else { (ox / 2, oy / 2) };
            let luma = if half {
                let (x, y) = (ox * 2, oy * 2);
                let sum = u32::from(data[y * stride + x])
                    + u32::from(data[y * stride + x + 1])
                    + u32::from(data[(y + 1) * stride + x])
                    + u32::from(data[(y + 1) * stride + x + 1]);
                (sum / 4) as i32
            } else {
                i32::from(data[oy * stride + ox])
            };
            let at = chroma + cy * stride + cx * 2;
            let (u, v) = (i32::from(data[at]) - 128, i32::from(data[at + 1]) - 128);
            let y = (luma - 16) * 1192;
            pixels.extend_from_slice(&[
                clamp(y + 1836 * v),
                clamp(y - 218 * u - 546 * v),
                clamp(y + 2163 * u),
                255,
            ]);
        }
    }
    Some(Rgba {
        width: out_w,
        height: out_h,
        pixels,
    })
}

/// From fixed point (10 fractional bits) to a byte, rounded.
fn clamp(value: i32) -> u8 {
    ((value + 512) >> 10).clamp(0, 255) as u8
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A flat NV12 picture of one colour.
    fn flat(width: usize, height: usize, y: u8, u: u8, v: u8) -> Vec<u8> {
        let mut data = vec![y; width * height];
        for _ in 0..width * height / 4 {
            data.extend_from_slice(&[u, v]);
        }
        data
    }

    #[test]
    fn black_white_and_red_convert() {
        let black = nv12_to_rgba(&flat(4, 4, 16, 128, 128), 4, 4, 4, false).unwrap();
        assert_eq!(&black.pixels[..4], &[0, 0, 0, 255]);
        let white = nv12_to_rgba(&flat(4, 4, 235, 128, 128), 4, 4, 4, false).unwrap();
        assert_eq!(&white.pixels[..4], &[255, 255, 255, 255]);
        // BT.709 red: Y 63, U 102, V 240.
        let red = nv12_to_rgba(&flat(4, 4, 63, 102, 240), 4, 4, 4, false).unwrap();
        assert!(red.pixels[0] > 250 && red.pixels[1] < 5 && red.pixels[2] < 5);
    }

    #[test]
    fn half_size_averages_luma() {
        let mut data = flat(4, 2, 16, 128, 128);
        data[1] = 235;
        data[4] = 235;
        data[5] = 235;
        let out = nv12_to_rgba(&data, 4, 2, 4, true).unwrap();
        assert_eq!((out.width, out.height), (2, 1));
        // Three of four are white: (235 * 3 + 16) / 4 = 180.
        let grey = clamp((180 - 16) * 1192);
        assert_eq!(&out.pixels[..4], &[grey, grey, grey, 255]);
    }

    #[test]
    fn strided_rows_are_read_within_the_stride() {
        let mut data = vec![16u8; 8 * 2];
        data.extend_from_slice(&[128, 128, 128, 128, 0, 0, 0, 0]);
        let out = nv12_to_rgba(&data, 4, 2, 8, false).unwrap();
        assert_eq!(out.pixels.len(), 4 * 2 * 4);
    }

    #[test]
    fn short_or_odd_buffers_are_refused() {
        let data = flat(4, 4, 16, 128, 128);
        assert!(nv12_to_rgba(&data[..data.len() - 1], 4, 4, 4, false).is_none());
        assert!(nv12_to_rgba(&data, 3, 4, 4, false).is_none());
        assert!(nv12_to_rgba(&data, 4, 4, 2, false).is_none());
        assert!(nv12_to_rgba(&[], 0, 0, 0, false).is_none());
    }
}
