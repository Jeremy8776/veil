//! Motion JPEG pictures from a camera, decoded to NV12.
//!
//! Purpose: many USB cameras on Linux offer 720p30 and above only as MJPEG. Each picture is a
//! JPEG, decoded here by zune-jpeg (pure Rust) to interleaved YCbCr and packed to NV12.
//!
//! Invariants:
//! - The picture comes from a device driver, so it is untrusted: a decoded size that differs
//!   from the mode the camera was set to is refused, and the decoder is told the largest size
//!   it may allocate for before it starts.
//! - Scratch and output buffers belong to the caller and are reused picture after picture.

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::MediaError;
use crate::yuv::ycbcr_to_nv12;

/// Decodes one JPEG of exactly `width` x `height` into packed NV12 in `out`.
pub fn mjpeg_to_nv12(
    jpeg: &[u8],
    width: u16,
    height: u16,
    scratch: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::YCbCr)
        .set_max_width(usize::from(width))
        .set_max_height(usize::from(height));
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    decoder
        .decode_headers()
        .map_err(|_| MediaError::Capture("camera JPEG"))?;
    let info = decoder.info().ok_or(MediaError::Capture("camera JPEG"))?;
    if (info.width, info.height) != (width, height) {
        return Err(MediaError::Capture("camera JPEG size"));
    }
    let size = decoder
        .output_buffer_size()
        .ok_or(MediaError::Capture("camera JPEG"))?;
    scratch.clear();
    scratch.resize(size, 0);
    decoder
        .decode_into(scratch)
        .map_err(|_| MediaError::Capture("camera JPEG"))?;
    ycbcr_to_nv12(scratch, width, height, out)
}

#[cfg(test)]
mod tests {
    use super::mjpeg_to_nv12;

    #[test]
    fn garbage_is_refused() {
        let (mut scratch, mut out) = (Vec::new(), Vec::new());
        assert!(mjpeg_to_nv12(&[], 4, 2, &mut scratch, &mut out).is_err());
        assert!(mjpeg_to_nv12(&[0xFF, 0xD8, 0, 1, 2, 3], 4, 2, &mut scratch, &mut out).is_err());
    }
}
