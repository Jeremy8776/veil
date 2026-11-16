//! Camera capture, the OS-independent part: choosing a camera mode and packing pictures.
//!
//! Purpose: every platform camera offers a list of native modes (size, frame rate, pixel
//! format). [`rank_modes`] orders them for what the ladder wants, and the platform code tries
//! them in that order. [`pack_nv12`] copies one NV12 picture out of a platform buffer with any
//! row pitch into a tightly packed one, so a [`crate::RawPicture`] never borrows OS memory.
//!
//! Invariants:
//! - The frame rate comes first: the ladder's temporal pattern assumes the capture clock, so a
//!   mode below the wanted rate is used only when no mode reaches it.
//! - Then size: the smallest mode that covers the wanted size, else the largest below it.
//!   Scaling only shrinks (see [`crate::scale`]), so a smaller camera lowers the top rung.
//! - Then the lowest frame rate that still meets the wanted one, then the pixel format that
//!   needs the least conversion (NV12, then YUY2, then MJPEG, then the rest).
//! - Packed output: luma rows of `width` bytes, then chroma rows of `width` bytes.

use std::cmp::Reverse;

use crate::MediaError;

/// The size and frame rate a caller asks a camera for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraMode {
    pub width: u16,
    pub height: u16,
    pub fps: u32,
}

/// A camera's pixel format, as far as the choice of mode cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PixelFormat {
    Nv12,
    Yuy2,
    Mjpeg,
    Other,
}

/// One mode a camera offers natively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeMode {
    pub width: u16,
    pub height: u16,
    /// Frame rate as a ratio, because cameras offer 30000/1001 and the like.
    pub fps_num: u32,
    pub fps_den: u32,
    pub format: PixelFormat,
}

impl NativeMode {
    /// Frames per second in thousandths, so 29.97 is 29_970.
    pub fn millifps(&self) -> u64 {
        if self.fps_den == 0 {
            return 0;
        }
        u64::from(self.fps_num) * 1_000 / u64::from(self.fps_den)
    }

    /// The frame rate rounded to whole frames per second.
    pub fn fps(&self) -> u32 {
        u32::try_from((self.millifps() + 500) / 1_000).unwrap_or(u32::MAX)
    }

    fn area(&self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }
}

/// Indexes into `offered`, best mode for `want` first. Modes with an odd or zero size are left
/// out, since NV12 needs even sizes.
pub fn rank_modes(offered: &[NativeMode], want: CameraMode) -> Vec<usize> {
    // 29.97 counts as 30: within half a frame per second.
    let wanted_millifps = u64::from(want.fps) * 1_000;
    let meets_fps = |m: &NativeMode| m.millifps() + 500 >= wanted_millifps;
    let covers = |m: &NativeMode| m.width >= want.width && m.height >= want.height;
    let mut ranked: Vec<usize> = (0..offered.len())
        .filter(|&i| {
            let m = &offered[i];
            m.width > 0 && m.height > 0 && m.width % 2 == 0 && m.height % 2 == 0
        })
        .collect();
    ranked.sort_by_key(|&i| {
        let m = &offered[i];
        let fps_ok = meets_fps(m);
        let size_ok = covers(m);
        (
            Reverse(fps_ok),
            Reverse(size_ok),
            // Covering: smallest first. Not covering: largest first.
            if size_ok {
                m.area()
            } else {
                u32::MAX - m.area()
            },
            // Fast enough: slowest first. Too slow: fastest first.
            if fps_ok {
                m.millifps()
            } else {
                u64::MAX - m.millifps()
            },
            m.format,
        )
    });
    ranked
}

/// Copies one NV12 picture from `src` into `out`, dropping row padding.
///
/// `pitch` is the bytes per row in `src`, `alloc_rows` the luma rows the buffer holds before
/// the chroma plane starts (at least `height`; drivers may pad it), and `width` x `height` the
/// picture to keep.
pub fn pack_nv12(
    src: &[u8],
    pitch: usize,
    alloc_rows: usize,
    width: u16,
    height: u16,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let (w, h) = (usize::from(width), usize::from(height));
    let even = w % 2 == 0 && h % 2 == 0;
    if w == 0 || h == 0 || !even || pitch < w || alloc_rows < h {
        return Err(MediaError::Malformed("NV12 picture size"));
    }
    let chroma_start = pitch
        .checked_mul(alloc_rows)
        .ok_or(MediaError::Malformed("NV12 picture size"))?;
    // The last chroma row needs only `w` bytes, not a full pitch.
    let needed = chroma_start
        .checked_add(pitch * (h / 2 - 1) + w)
        .ok_or(MediaError::Malformed("NV12 picture size"))?;
    if src.len() < needed {
        return Err(MediaError::Malformed("NV12 buffer"));
    }
    out.clear();
    out.reserve(w * h * 3 / 2);
    for row in 0..h {
        out.extend_from_slice(&src[row * pitch..row * pitch + w]);
    }
    for row in 0..h / 2 {
        let start = chroma_start + row * pitch;
        out.extend_from_slice(&src[start..start + w]);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn mode(width: u16, height: u16, fps: u32, format: PixelFormat) -> NativeMode {
        NativeMode {
            width,
            height,
            fps_num: fps,
            fps_den: 1,
            format,
        }
    }

    const UHD60: CameraMode = CameraMode {
        width: 3_840,
        height: 2_160,
        fps: 60,
    };

    #[test]
    fn frame_rate_beats_size() {
        let offered = [
            mode(3_840, 2_160, 30, PixelFormat::Mjpeg),
            mode(1_920, 1_080, 60, PixelFormat::Mjpeg),
        ];
        assert_eq!(rank_modes(&offered, UHD60), [1, 0]);
    }

    #[test]
    fn smallest_covering_size_then_largest_below() {
        let offered = [
            mode(640, 480, 60, PixelFormat::Yuy2),
            mode(4_096, 2_160, 60, PixelFormat::Mjpeg),
            mode(3_840, 2_160, 60, PixelFormat::Mjpeg),
            mode(1_920, 1_080, 60, PixelFormat::Mjpeg),
        ];
        assert_eq!(rank_modes(&offered, UHD60), [2, 1, 3, 0]);
    }

    #[test]
    fn slowest_sufficient_rate_then_cheapest_format() {
        let offered = [
            mode(1_920, 1_080, 120, PixelFormat::Nv12),
            mode(1_920, 1_080, 60, PixelFormat::Mjpeg),
            mode(1_920, 1_080, 60, PixelFormat::Nv12),
            mode(1_920, 1_080, 60, PixelFormat::Other),
        ];
        let want = CameraMode {
            width: 1_920,
            height: 1_080,
            fps: 60,
        };
        assert_eq!(rank_modes(&offered, want), [2, 1, 3, 0]);
    }

    #[test]
    fn ntsc_rate_counts_as_whole_and_odd_sizes_are_dropped() {
        let ntsc = NativeMode {
            fps_num: 30_000,
            fps_den: 1_001,
            ..mode(1_280, 720, 0, PixelFormat::Nv12)
        };
        assert_eq!(ntsc.fps(), 30);
        let offered = [mode(1_281, 720, 30, PixelFormat::Nv12), ntsc];
        let want = CameraMode {
            width: 1_280,
            height: 720,
            fps: 30,
        };
        assert_eq!(rank_modes(&offered, want), [1]);
    }

    #[test]
    fn packing_drops_pitch_and_row_padding() {
        // 2x2 picture, pitch 3, buffer holds 4 luma rows before chroma.
        let src = [1, 2, 0, 3, 4, 0, 9, 9, 9, 9, 9, 9, 5, 6];
        let mut out = Vec::new();
        pack_nv12(&src, 3, 4, 2, 2, &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn packing_refuses_short_buffers_and_bad_sizes() {
        let mut out = Vec::new();
        assert!(pack_nv12(&[0; 5], 2, 2, 2, 2, &mut out).is_err());
        assert!(pack_nv12(&[0; 6], 2, 2, 2, 2, &mut out).is_ok());
        assert!(pack_nv12(&[0; 6], 1, 2, 2, 2, &mut out).is_err());
        assert!(pack_nv12(&[0; 6], 2, 1, 2, 2, &mut out).is_err());
        assert!(pack_nv12(&[0; 6], 2, 2, 3, 2, &mut out).is_err());
    }
}
