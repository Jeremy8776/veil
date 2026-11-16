//! The interface every platform video decoder implements.
//!
//! Purpose: the receiving side of [`crate::VideoEncoder`]. Media Foundation, VideoToolbox and
//! VA-API decoders all take one access unit (the bitstream after the [`crate::VideoHeader`])
//! and give back at most one NV12 picture, tightly packed, so the renderer never knows which
//! one runs.
//!
//! Invariants:
//! - Call [`VideoDecoder::decode`] only for a layer [`crate::Gate`] said to decode. The gate
//!   makes sure the reference picture was decoded first, so the decoder never has to guess.
//! - One decoder per sender and spatial layer. A change of layer starts at a keyframe, which
//!   carries the new size, so the decoder follows without being reopened.
//! - The bitstream comes from another member. A decoder refuses a picture larger than
//!   [`MAX_DECODED_PIXELS`] rather than allocate for it.
//! - The returned picture borrows the decoder's own buffer and is valid until the next call.

use crate::MediaError;
use crate::encoder::RawPicture;

/// The largest picture a decoder hands back: 4K, with the 16 rows of padding H.264 adds.
pub const MAX_DECODED_PIXELS: u32 = 3_840 * 2_176;

pub trait VideoDecoder {
    /// Decodes one access unit. `None` means the decoder took it but has no picture yet (it
    /// has not seen a keyframe, or it needs more input).
    fn decode(
        &mut self,
        bitstream: &[u8],
        timestamp_us: u64,
    ) -> Result<Option<RawPicture<'_>>, MediaError>;
}

/// Refuses a decoded size that is zero, odd, or larger than [`MAX_DECODED_PIXELS`].
pub fn check_size(width: u32, height: u32) -> Result<(u16, u16), MediaError> {
    let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err(MediaError::Decoder("picture too large"));
    };
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
        return Err(MediaError::Decoder("picture size"));
    }
    if width.saturating_mul(height) > MAX_DECODED_PIXELS {
        return Err(MediaError::Decoder("picture too large"));
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_up_to_padded_4k_pass() {
        assert_eq!(check_size(1_920, 1_088), Ok((1_920, 1_088)));
        assert_eq!(check_size(3_840, 2_176), Ok((3_840, 2_176)));
    }

    #[test]
    fn odd_empty_and_huge_sizes_are_refused() {
        assert!(check_size(0, 360).is_err());
        assert!(check_size(641, 360).is_err());
        assert!(check_size(3_840, 2_178).is_err());
        assert!(check_size(70_000, 2).is_err());
        assert!(check_size(7_680, 4_320).is_err());
    }
}
