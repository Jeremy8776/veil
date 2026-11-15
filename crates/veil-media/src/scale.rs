//! Shrinks an NV12 picture to a ladder rung's size.
//!
//! Purpose: simulcast feeds every rung's encoder its own size from one captured picture.
//! Each output pixel is the average of the source pixels it covers (a box filter), which is
//! enough for a downscale and needs no platform code.
//!
//! Invariants:
//! - Output is tightly packed: stride equals width, luma plane then interleaved chroma plane.
//! - Only shrinks or copies. A rung larger than the picture is refused by the caller.
//!
//! shortcut: CPU box filter, about one read per source pixel per rung. Move scaling to the GPU
//! (the D3D11 video processor on Windows) when capture hands over GPU textures.

use crate::RawPicture;

/// Writes `picture` scaled to `width` x `height` into `out`, replacing its contents.
pub fn scale_nv12(picture: &RawPicture<'_>, width: u16, height: u16, out: &mut Vec<u8>) {
    let (sw, sh) = (usize::from(picture.width), usize::from(picture.height));
    let (dw, dh) = (usize::from(width), usize::from(height));
    out.clear();
    out.reserve(dw * dh * 3 / 2);
    let luma = &picture.data[..picture.stride * sh];
    let chroma = &picture.data[picture.stride * sh..];
    plane(luma, picture.stride, (sw, sh), (dw, dh), 1, out);
    plane(
        chroma,
        picture.stride,
        (sw / 2, sh / 2),
        (dw / 2, dh / 2),
        2,
        out,
    );
}

/// Box-filters one plane of `channels` interleaved samples per pixel.
fn plane(
    src: &[u8],
    stride: usize,
    from: (usize, usize),
    to: (usize, usize),
    channels: usize,
    out: &mut Vec<u8>,
) {
    let (sw, sh) = from;
    let (dw, dh) = to;
    if (sw, sh) == (dw, dh) {
        for row in src.chunks(stride).take(sh) {
            out.extend_from_slice(&row[..sw * channels]);
        }
        return;
    }
    for y in 0..dh {
        let (y0, y1) = span(y, sh, dh);
        for x in 0..dw {
            let (x0, x1) = span(x, sw, dw);
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            for c in 0..channels {
                let mut sum = 0u32;
                for row in y0..y1 {
                    let line = &src[row * stride..];
                    for col in x0..x1 {
                        sum += u32::from(line[col * channels + c]);
                    }
                }
                out.push(((sum + count / 2) / count) as u8);
            }
        }
    }
}

/// The source range output index `i` covers, at least one sample wide.
fn span(i: usize, source: usize, dest: usize) -> (usize, usize) {
    let start = i * source / dest;
    let end = ((i + 1) * source / dest).max(start + 1).min(source);
    (start, end)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn same_size_drops_the_stride_padding() {
        // 2x2 picture, stride 3: luma rows [1,2,x] [3,4,x], chroma row [5,6,x].
        let data = [1, 2, 0, 3, 4, 0, 5, 6, 0];
        let picture = RawPicture::new(2, 2, 3, 0, &data).unwrap();
        let mut out = Vec::new();
        scale_nv12(&picture, 2, 2, &mut out);
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn halving_averages_each_block() {
        // 4x4 luma of 0 and 200 in 2x2 blocks; chroma 2x2 pixels of (U, V).
        let mut data = vec![0u8; 4 * 6];
        for (i, value) in data.iter_mut().take(16).enumerate() {
            *value = if (i % 4 < 2) == (i / 4 < 2) { 200 } else { 0 };
        }
        data[16..24].copy_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80]);
        let picture = RawPicture::new(4, 4, 4, 0, &data).unwrap();
        let mut out = Vec::new();
        scale_nv12(&picture, 2, 2, &mut out);
        assert_eq!(out[..4], [200, 0, 0, 200]);
        // One chroma pixel: the mean U and mean V of the four.
        assert_eq!(out[4..], [40, 50]);
    }
}
