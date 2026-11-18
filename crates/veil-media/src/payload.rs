//! The header in front of every encoded video layer, inside the sealed frame.
//!
//! Purpose: a receiver needs to know what a layer refers to before it hands it to a decoder.
//! The plaintext layer marker (spatial, temporal, keyframe) is outside, so a forwarder can read
//! it. Everything else is in here, encrypted: forwarders never see picture numbers or sizes.
//!
//! Wire format (version 1, big-endian), then the codec's bitstream:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0 | version, 1 |
//! | 1 | codec: 0 H.264, 1 HEVC, 2 AV1 |
//! | 2 | flags: bit 0 `needs_lower`, bit 1 `screen`, every other bit zero |
//! | 3..11 | picture number on the sender's capture timeline |
//! | 11..15 | distance back to the reference picture; 0 for a keyframe |
//! | 15..23 | capture time in microseconds, sender's own clock |
//! | 23..25 | width |
//! | 25..27 | height |
//!
//! Invariants:
//! - A header parses only if every field is in range, so a bad member cannot make a receiver
//!   read past the buffer or allocate.
//! - A layer's reference is on the same spatial layer. `needs_lower` means it also needs the
//!   spatial layer below for the same picture (SVC). Simulcast layers never set it.
//! - `screen` marks a shared screen, a stream of its own beside the camera. It is inside the
//!   sealed frame, so a forwarder cannot tell. A version without it refuses these headers and
//!   so ignores a share rather than showing it as the camera.

use veil_security::frame::{HEADER_LEN, MAX_FRAME_LEN, TAG_LEN};

use crate::MediaError;
use crate::ladder::MAX_DIMENSION;

pub const VERSION: u8 = 1;
pub const VIDEO_HEADER_LEN: usize = 27;
const NEEDS_LOWER: u8 = 0b1;
const SCREEN: u8 = 0b10;

/// Largest bitstream one sealed frame can carry after both headers and the tag.
pub const MAX_BITSTREAM_LEN: usize = MAX_FRAME_LEN - VIDEO_HEADER_LEN - HEADER_LEN - TAG_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    fn byte(self) -> u8 {
        match self {
            Self::H264 => 0,
            Self::Hevc => 1,
            Self::Av1 => 2,
        }
    }

    fn from_byte(byte: u8) -> Result<Self, MediaError> {
        match byte {
            0 => Ok(Self::H264),
            1 => Ok(Self::Hevc),
            2 => Ok(Self::Av1),
            _ => Err(MediaError::Malformed("codec")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoHeader {
    pub codec: Codec,
    pub picture: u64,
    /// The picture this layer is predicted from, on the same spatial layer. `None` for a
    /// keyframe.
    pub reference: Option<u64>,
    /// Also needs the spatial layer below for the same picture.
    pub needs_lower: bool,
    /// This picture belongs to the sender's shared screen, not their camera.
    pub screen: bool,
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
}

impl VideoHeader {
    /// The header followed by `bitstream`, ready to seal.
    pub fn write(&self, bitstream: &[u8]) -> Result<Vec<u8>, MediaError> {
        if bitstream.len() > MAX_BITSTREAM_LEN {
            return Err(MediaError::TooLarge);
        }
        let distance = match self.reference {
            None => 0,
            Some(r) if r < self.picture => u32::try_from(self.picture - r)
                .map_err(|_| MediaError::Malformed("reference distance"))?,
            Some(_) => return Err(MediaError::Malformed("reference after picture")),
        };
        check_size(self.width, self.height)?;
        let mut out = Vec::with_capacity(VIDEO_HEADER_LEN + bitstream.len());
        out.push(VERSION);
        out.push(self.codec.byte());
        let lower = if self.needs_lower { NEEDS_LOWER } else { 0 };
        out.push(lower | if self.screen { SCREEN } else { 0 });
        out.extend_from_slice(&self.picture.to_be_bytes());
        out.extend_from_slice(&distance.to_be_bytes());
        out.extend_from_slice(&self.timestamp_us.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
        out.extend_from_slice(bitstream);
        Ok(out)
    }

    /// Splits an opened frame into its header and bitstream.
    pub fn read(payload: &[u8]) -> Result<(Self, &[u8]), MediaError> {
        let (head, bitstream) = payload
            .split_at_checked(VIDEO_HEADER_LEN)
            .ok_or(MediaError::Malformed("short payload"))?;
        if head[0] != VERSION {
            return Err(MediaError::Malformed("version"));
        }
        let codec = Codec::from_byte(head[1])?;
        if head[2] & !(NEEDS_LOWER | SCREEN) != 0 {
            return Err(MediaError::Malformed("flags"));
        }
        let picture = u64::from_be_bytes(array(&head[3..11])?);
        let distance = u32::from_be_bytes(array(&head[11..15])?);
        let reference = match distance {
            0 => None,
            d => Some(
                picture
                    .checked_sub(u64::from(d))
                    .ok_or(MediaError::Malformed("reference before stream"))?,
            ),
        };
        let width = u16::from_be_bytes(array(&head[23..25])?);
        let height = u16::from_be_bytes(array(&head[25..27])?);
        check_size(width, height)?;
        let header = Self {
            codec,
            picture,
            reference,
            needs_lower: head[2] & NEEDS_LOWER != 0,
            screen: head[2] & SCREEN != 0,
            timestamp_us: u64::from_be_bytes(array(&head[15..23])?),
            width,
            height,
        };
        Ok((header, bitstream))
    }
}

fn check_size(width: u16, height: u16) -> Result<(), MediaError> {
    let fits = (1..=MAX_DIMENSION).contains(&width) && (1..=MAX_DIMENSION).contains(&height);
    if fits {
        Ok(())
    } else {
        Err(MediaError::Malformed("picture size"))
    }
}

fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], MediaError> {
    bytes.try_into().map_err(|_| MediaError::Malformed("field"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn header() -> VideoHeader {
        VideoHeader {
            codec: Codec::Av1,
            picture: 1_000,
            reference: Some(996),
            needs_lower: true,
            screen: false,
            timestamp_us: 16_667,
            width: 1_920,
            height: 1_080,
        }
    }

    #[test]
    fn header_and_bitstream_round_trip() {
        let bytes = header().write(b"obu").unwrap();
        assert_eq!(bytes.len(), VIDEO_HEADER_LEN + 3);
        let (read, bitstream) = VideoHeader::read(&bytes).unwrap();
        assert_eq!(read, header());
        assert_eq!(bitstream, b"obu");

        let key = VideoHeader {
            reference: None,
            needs_lower: false,
            ..header()
        };
        assert_eq!(VideoHeader::read(&key.write(&[]).unwrap()).unwrap().0, key);
    }

    #[test]
    fn the_screen_mark_round_trips_and_stays_off_the_camera() {
        let screen = VideoHeader {
            screen: true,
            ..header()
        };
        let bytes = screen.write(b"x").unwrap();
        assert_eq!(bytes[2], NEEDS_LOWER | SCREEN);
        assert!(VideoHeader::read(&bytes).unwrap().0.screen);
        let camera = header().write(b"x").unwrap();
        assert!(!VideoHeader::read(&camera).unwrap().0.screen);
    }

    #[test]
    fn bad_headers_are_refused() {
        let good = header().write(b"x").unwrap();
        let mut bad = good.clone();
        bad[0] = 2;
        assert!(VideoHeader::read(&bad).is_err(), "version");
        let mut bad = good.clone();
        bad[1] = 9;
        assert!(VideoHeader::read(&bad).is_err(), "codec");
        let mut bad = good.clone();
        bad[2] = 0b100;
        assert!(VideoHeader::read(&bad).is_err(), "flags");
        let mut bad = good.clone();
        bad[23..25].copy_from_slice(&0u16.to_be_bytes());
        assert!(VideoHeader::read(&bad).is_err(), "zero width");
        let mut bad = good.clone();
        bad[11..15].copy_from_slice(&2_000u32.to_be_bytes());
        assert!(
            VideoHeader::read(&bad).is_err(),
            "reference before picture 0"
        );
        assert!(
            VideoHeader::read(&good[..VIDEO_HEADER_LEN - 1]).is_err(),
            "short"
        );
    }

    #[test]
    fn writing_refuses_impossible_headers() {
        let ahead = VideoHeader {
            reference: Some(1_000),
            ..header()
        };
        assert!(ahead.write(&[]).is_err());
        let huge = VideoHeader {
            width: MAX_DIMENSION + 2,
            ..header()
        };
        assert!(huge.write(&[]).is_err());
    }
}
