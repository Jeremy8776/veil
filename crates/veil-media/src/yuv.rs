//! Pixel layout conversions between NV12 and the formats other platforms use.
//!
//! Purpose: the pipeline carries tightly packed NV12 (see [`crate::RawPicture`]). OpenH264
//! takes and gives I420 (three planes), Linux cameras send YUYV or MJPEG (decoded to
//! interleaved YCbCr), and macOS hands NV12 as two separately pitched planes. These functions
//! convert each into packed NV12 and back, in safe code, so the platform modules stay small.
//!
//! Invariants:
//! - Width and height are even and non-zero, as NV12 needs; anything else is refused.
//! - Every source slice is checked to hold the whole picture before it is read, so a short
//!   buffer from a driver or a decoder is an error, never a panic.
//! - Output is written into the caller's `Vec`, replacing its contents, so a long-lived
//!   buffer is reused picture after picture.
//! - Chroma that is subsampled here (YUYV, 4:4:4) is the average of the samples it covers.

use crate::MediaError;

/// Checks an NV12-friendly size and returns it as `usize`.
fn even_size(width: u16, height: u16) -> Result<(usize, usize), MediaError> {
    let ok = width > 0 && height > 0 && width.is_multiple_of(2) && height.is_multiple_of(2);
    if ok {
        Ok((usize::from(width), usize::from(height)))
    } else {
        Err(MediaError::Malformed("picture size"))
    }
}

/// Checks `data` holds `rows` rows of `row_len` bytes at `pitch` bytes per row.
fn fits(data: &[u8], pitch: usize, row_len: usize, rows: usize) -> Result<(), MediaError> {
    if rows == 0 {
        return Ok(());
    }
    let needed = pitch
        .checked_mul(rows - 1)
        .and_then(|n| n.checked_add(row_len))
        .ok_or(MediaError::Malformed("picture size"))?;
    if pitch < row_len || data.len() < needed {
        return Err(MediaError::Malformed("picture buffer"));
    }
    Ok(())
}

/// Splits packed NV12 into packed I420: Y plane, then U, then V, each tightly packed.
pub fn nv12_to_i420(nv12: &[u8], width: u16, height: u16, out: &mut Vec<u8>) -> Result<(), MediaError> {
    let (w, h) = even_size(width, height)?;
    fits(nv12, w, w, h + h / 2)?;
    let (luma, chroma) = nv12.split_at(w * h);
    out.clear();
    out.reserve(w * h * 3 / 2);
    out.extend_from_slice(luma);
    let chroma = &chroma[..w * h / 2];
    out.extend(chroma.iter().step_by(2));
    out.extend(chroma.iter().skip(1).step_by(2));
    Ok(())
}

/// One plane of an I420 picture: its bytes and its row pitch.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub pitch: usize,
}

/// Joins three I420 planes (any pitch) into packed NV12.
pub fn i420_to_nv12(
    y: Plane<'_>,
    u: Plane<'_>,
    v: Plane<'_>,
    width: u16,
    height: u16,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let (w, h) = even_size(width, height)?;
    let (cw, ch) = (w / 2, h / 2);
    fits(y.data, y.pitch, w, h)?;
    fits(u.data, u.pitch, cw, ch)?;
    fits(v.data, v.pitch, cw, ch)?;
    out.clear();
    out.reserve(w * h * 3 / 2);
    for row in 0..h {
        out.extend_from_slice(&y.data[row * y.pitch..row * y.pitch + w]);
    }
    for row in 0..ch {
        let u_row = &u.data[row * u.pitch..row * u.pitch + cw];
        let v_row = &v.data[row * v.pitch..row * v.pitch + cw];
        for (cb, cr) in u_row.iter().zip(v_row) {
            out.push(*cb);
            out.push(*cr);
        }
    }
    Ok(())
}

/// Joins a separately pitched luma plane and interleaved chroma plane into packed NV12.
pub fn planes_to_nv12(
    luma: Plane<'_>,
    chroma: Plane<'_>,
    width: u16,
    height: u16,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let (w, h) = even_size(width, height)?;
    fits(luma.data, luma.pitch, w, h)?;
    fits(chroma.data, chroma.pitch, w, h / 2)?;
    out.clear();
    out.reserve(w * h * 3 / 2);
    for row in 0..h {
        out.extend_from_slice(&luma.data[row * luma.pitch..row * luma.pitch + w]);
    }
    for row in 0..h / 2 {
        out.extend_from_slice(&chroma.data[row * chroma.pitch..row * chroma.pitch + w]);
    }
    Ok(())
}

fn avg4(a: u8, b: u8, c: u8, d: u8) -> u8 {
    // At most 4 * 255 + 2, so the sum fits and the result is at most 255.
    u8::try_from((u16::from(a) + u16::from(b) + u16::from(c) + u16::from(d) + 2) / 4)
        .unwrap_or(u8::MAX)
}

fn avg2(a: u8, b: u8) -> u8 {
    u8::try_from((u16::from(a) + u16::from(b) + 1) / 2).unwrap_or(u8::MAX)
}

/// Converts YUYV (YUY2: Y0 U Y1 V per two pixels) with any pitch into packed NV12.
pub fn yuyv_to_nv12(
    src: &[u8],
    pitch: usize,
    width: u16,
    height: u16,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let (w, h) = even_size(width, height)?;
    fits(src, pitch, w * 2, h)?;
    out.clear();
    out.reserve(w * h * 3 / 2);
    for row in 0..h {
        let line = &src[row * pitch..row * pitch + w * 2];
        out.extend(line.iter().step_by(2));
    }
    for pair in 0..h / 2 {
        let top = &src[2 * pair * pitch..2 * pair * pitch + w * 2];
        let bottom = &src[(2 * pair + 1) * pitch..(2 * pair + 1) * pitch + w * 2];
        for (t, b) in top.chunks_exact(4).zip(bottom.chunks_exact(4)) {
            out.push(avg2(t[1], b[1]));
            out.push(avg2(t[3], b[3]));
        }
    }
    Ok(())
}

/// Converts interleaved full-resolution YCbCr (3 bytes per pixel, tightly packed, as a JPEG
/// decoder gives it) into packed NV12.
pub fn ycbcr_to_nv12(src: &[u8], width: u16, height: u16, out: &mut Vec<u8>) -> Result<(), MediaError> {
    let (w, h) = even_size(width, height)?;
    fits(src, w * 3, w * 3, h)?;
    out.clear();
    out.reserve(w * h * 3 / 2);
    out.extend(src[..w * h * 3].iter().step_by(3));
    for pair in 0..h / 2 {
        let top = &src[2 * pair * w * 3..(2 * pair + 1) * w * 3];
        let bottom = &src[(2 * pair + 1) * w * 3..(2 * pair + 2) * w * 3];
        for (t, b) in top.chunks_exact(6).zip(bottom.chunks_exact(6)) {
            out.push(avg4(t[1], t[4], b[1], b[4]));
            out.push(avg4(t[2], t[5], b[2], b[5]));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A 4x2 NV12 picture: luma 0..8, chroma (U, V) pairs (100, 200) and (101, 201).
    fn nv12_4x2() -> Vec<u8> {
        vec![0, 1, 2, 3, 4, 5, 6, 7, 100, 200, 101, 201]
    }

    #[test]
    fn nv12_and_i420_round_trip() {
        let mut i420 = Vec::new();
        nv12_to_i420(&nv12_4x2(), 4, 2, &mut i420).unwrap();
        assert_eq!(i420, [0, 1, 2, 3, 4, 5, 6, 7, 100, 101, 200, 201]);
        let mut back = Vec::new();
        i420_to_nv12(
            Plane { data: &i420[..8], pitch: 4 },
            Plane { data: &i420[8..10], pitch: 2 },
            Plane { data: &i420[10..], pitch: 2 },
            4,
            2,
            &mut back,
        )
        .unwrap();
        assert_eq!(back, nv12_4x2());
    }

    #[test]
    fn i420_with_padded_rows_is_packed() {
        // Pitch 6 for luma, 4 for chroma; padding bytes are 9 and must not appear.
        let y = [0, 1, 2, 3, 9, 9, 4, 5, 6, 7];
        let u = [100, 101];
        let v = [200, 201];
        let mut out = Vec::new();
        i420_to_nv12(
            Plane { data: &y, pitch: 6 },
            Plane { data: &u, pitch: 4 },
            Plane { data: &v, pitch: 4 },
            4,
            2,
            &mut out,
        )
        .unwrap();
        assert_eq!(out, nv12_4x2());
    }

    #[test]
    fn separate_planes_are_packed() {
        let luma = [0, 1, 2, 3, 9, 4, 5, 6, 7];
        let chroma = [100, 200, 101, 201];
        let mut out = Vec::new();
        planes_to_nv12(
            Plane { data: &luma, pitch: 5 },
            Plane { data: &chroma, pitch: 4 },
            4,
            2,
            &mut out,
        )
        .unwrap();
        assert_eq!(out, nv12_4x2());
    }

    #[test]
    fn yuyv_keeps_luma_and_averages_chroma_rows() {
        // Row 0: Y 0 1 2 3, U 100/102, V 200/202. Row 1: Y 4 5 6 7, U 102/104, V 202/204.
        // One padding byte per row (pitch 9).
        let src = [
            0, 100, 1, 200, 2, 102, 3, 202, 9, //
            4, 102, 5, 202, 6, 104, 7, 204, 9,
        ];
        let mut out = Vec::new();
        yuyv_to_nv12(&src, 9, 4, 2, &mut out).unwrap();
        assert_eq!(out, [0, 1, 2, 3, 4, 5, 6, 7, 101, 201, 103, 203]);
    }

    #[test]
    fn ycbcr_averages_each_two_by_two_block() {
        let mut src = Vec::new();
        for (i, y) in (0u8..8).enumerate() {
            let cb = if i % 4 < 2 { 100 } else { 104 };
            src.extend_from_slice(&[y, cb, cb + 100]);
        }
        let mut out = Vec::new();
        ycbcr_to_nv12(&src, 4, 2, &mut out).unwrap();
        assert_eq!(out, [0, 1, 2, 3, 4, 5, 6, 7, 100, 200, 104, 204]);
    }

    #[test]
    fn short_buffers_and_odd_sizes_are_refused() {
        let mut out = Vec::new();
        assert!(nv12_to_i420(&nv12_4x2()[..11], 4, 2, &mut out).is_err());
        assert!(nv12_to_i420(&nv12_4x2(), 3, 2, &mut out).is_err());
        assert!(yuyv_to_nv12(&[0; 15], 8, 4, 2, &mut out).is_err());
        assert!(yuyv_to_nv12(&[0; 16], 6, 4, 2, &mut out).is_err());
        assert!(ycbcr_to_nv12(&[0; 23], 4, 2, &mut out).is_err());
        let y = [0u8; 8];
        let c = [0u8; 1];
        let plane = |data| Plane { data, pitch: 2 };
        assert!(i420_to_nv12(Plane { data: &y, pitch: 4 }, plane(&c), plane(&c), 4, 2, &mut out).is_err());
        assert!(planes_to_nv12(Plane { data: &y, pitch: 4 }, Plane { data: &y[..3], pitch: 4 }, 4, 2, &mut out).is_err());
    }
}
