//! The interface every platform video encoder implements.
//!
//! Purpose: Media Foundation, VideoToolbox and VA-API encoders all take the same input (one
//! NV12 picture plus the [`PicturePlan`]) and give the same output (one bitstream per planned
//! layer). The rest of the pipeline never knows which one runs.
//!
//! Invariants:
//! - An encoder returns exactly one [`EncodedLayer`] per layer in the plan, in the same order.
//!   [`check_output`] enforces this before anything is sealed.
//! - A keyframe in the plan must come out as a keyframe; the encoder is told, not asked.

use veil_security::Layer;

use crate::MediaError;
use crate::payload::Codec;
use crate::schedule::PicturePlan;

/// One captured picture in NV12: a full-size luma plane, then a half-size interleaved chroma
/// plane, each row `stride` bytes.
pub struct RawPicture<'a> {
    pub width: u16,
    pub height: u16,
    pub stride: usize,
    pub timestamp_us: u64,
    pub data: &'a [u8],
}

impl<'a> RawPicture<'a> {
    /// Checks the buffer holds the whole picture before any encoder reads it.
    pub fn new(
        width: u16,
        height: u16,
        stride: usize,
        timestamp_us: u64,
        data: &'a [u8],
    ) -> Result<Self, MediaError> {
        let even = width % 2 == 0 && height % 2 == 0;
        if width == 0 || height == 0 || !even || stride < usize::from(width) {
            return Err(MediaError::Malformed("picture size"));
        }
        let rows = usize::from(height) + usize::from(height) / 2;
        let needed = stride
            .checked_mul(rows)
            .ok_or(MediaError::Malformed("picture size"))?;
        if data.len() < needed {
            return Err(MediaError::Malformed("picture buffer"));
        }
        Ok(Self {
            width,
            height,
            stride,
            timestamp_us,
            data,
        })
    }
}

/// One spatial layer's bitstream for one picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedLayer {
    pub layer: Layer,
    pub width: u16,
    pub height: u16,
    /// Predicted from the layer below for the same picture (SVC). False for simulcast.
    pub needs_lower: bool,
    pub bitstream: Vec<u8>,
}

pub trait VideoEncoder {
    fn codec(&self) -> Codec;

    /// Encodes `picture` into the layers `plan` lists. Not called for a plan with no layers.
    fn encode(
        &mut self,
        picture: &RawPicture<'_>,
        plan: &PicturePlan,
    ) -> Result<Vec<EncodedLayer>, MediaError>;
}

/// Refuses encoder output that does not match the plan layer for layer.
pub fn check_output(plan: &PicturePlan, output: &[EncodedLayer]) -> Result<(), MediaError> {
    let matches = output.len() == plan.layers.len()
        && output
            .iter()
            .zip(&plan.layers)
            .all(|(out, planned)| out.layer == *planned);
    if matches {
        Ok(())
    } else {
        Err(MediaError::Encoder("output does not match the plan"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::ladder::Ladder;
    use crate::payload::VideoHeader;
    use crate::schedule::Schedule;
    use crate::{Gate, Verdict};

    /// Stands in for a platform encoder: the bitstream is the picture's first luma byte.
    struct Echo;

    impl VideoEncoder for Echo {
        fn codec(&self) -> Codec {
            Codec::H264
        }

        fn encode(
            &mut self,
            picture: &RawPicture<'_>,
            plan: &PicturePlan,
        ) -> Result<Vec<EncodedLayer>, MediaError> {
            Ok(plan
                .layers
                .iter()
                .map(|layer| EncodedLayer {
                    layer: *layer,
                    width: picture.width,
                    height: picture.height,
                    needs_lower: false,
                    bitstream: picture.data[..1].to_vec(),
                })
                .collect())
        }
    }

    #[test]
    fn pictures_must_fit_their_buffer() {
        let data = vec![0u8; 4 * 6];
        assert!(RawPicture::new(4, 4, 4, 0, &data).is_ok());
        assert!(RawPicture::new(4, 4, 4, 0, &data[..23]).is_err());
        assert!(RawPicture::new(3, 4, 4, 0, &data).is_err());
        assert!(RawPicture::new(4, 4, 2, 0, &data).is_err());
    }

    #[test]
    fn schedule_encoder_header_and_gate_fit_together() {
        let now = Instant::now();
        let mut schedule = Schedule::new(Ladder::camera());
        let mut encoder = Echo;
        let mut gates = Gate::default();
        let data = vec![7u8; 4 * 6];
        for _ in 0..8 {
            let plan = schedule.next(now);
            let picture = RawPicture::new(4, 4, 4, plan.picture, &data).unwrap();
            let out = encoder.encode(&picture, &plan).unwrap();
            check_output(&plan, &out).unwrap();
            for layer in out {
                let header = VideoHeader {
                    codec: encoder.codec(),
                    picture: plan.picture,
                    reference: plan.reference,
                    needs_lower: layer.needs_lower,
                    timestamp_us: picture.timestamp_us,
                    width: layer.width,
                    height: layer.height,
                };
                let payload = header.write(&layer.bitstream).unwrap();
                let (read, bitstream) = VideoHeader::read(&payload).unwrap();
                assert_eq!(bitstream, [7]);
                assert_eq!(gates.admit(layer.layer, &read), Verdict::Decode);
            }
        }
    }

    #[test]
    fn mismatched_output_is_refused() {
        let mut schedule = Schedule::new(Ladder::camera());
        let plan = schedule.next(Instant::now());
        assert!(check_output(&plan, &[]).is_err());
    }
}
