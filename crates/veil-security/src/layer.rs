//! The plaintext layer marker on every sealed frame.
//!
//! Purpose: a forwarder must drop SVC layers a receiver did not ask for, without decrypting.
//! So each sealed frame carries two plain bytes after the key id and counter, and they are part
//! of the AEAD's associated data: a forwarder can read them but not change them.
//!
//! Wire format:
//! - byte 0: flags. Bit 0 is `keyframe`. Every other bit must be zero.
//! - byte 1: spatial layer in the high nibble (0 to [`MAX_SPATIAL`]), temporal layer in the low
//!   nibble (0 to [`MAX_TEMPORAL`]).
//!
//! Audio and single-layer video use [`Layer::BASE`].

use crate::SecurityError;

pub const LAYER_LEN: usize = 2;
/// Thumb, Hd and Uhd.
pub const MAX_SPATIAL: u8 = 2;
/// Up to four frame-rate steps.
pub const MAX_TEMPORAL: u8 = 3;
const KEYFRAME: u8 = 0b1;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Layer {
    /// 0 is the smallest picture. Higher layers need every lower one to decode.
    pub spatial: u8,
    /// 0 is the lowest frame rate. Higher layers need every lower one to decode.
    pub temporal: u8,
    /// A receiver can start decoding this layer here without earlier frames.
    pub keyframe: bool,
}

impl Layer {
    pub const BASE: Self = Self {
        spatial: 0,
        temporal: 0,
        keyframe: false,
    };

    pub fn new(spatial: u8, temporal: u8, keyframe: bool) -> Result<Self, SecurityError> {
        if spatial > MAX_SPATIAL {
            return Err(SecurityError::Malformed("spatial layer"));
        }
        if temporal > MAX_TEMPORAL {
            return Err(SecurityError::Malformed("temporal layer"));
        }
        Ok(Self {
            spatial,
            temporal,
            keyframe,
        })
    }

    pub fn encode(self) -> [u8; LAYER_LEN] {
        let flags = if self.keyframe { KEYFRAME } else { 0 };
        [flags, (self.spatial << 4) | self.temporal]
    }

    pub fn decode(bytes: [u8; LAYER_LEN]) -> Result<Self, SecurityError> {
        let [flags, layers] = bytes;
        if flags & !KEYFRAME != 0 {
            return Err(SecurityError::Malformed("layer flags"));
        }
        Self::new(layers >> 4, layers & 0x0F, flags & KEYFRAME != 0)
    }
}

/// The highest layers a receiver wants from one sender. Forwarders filter by it, and senders
/// use the highest anyone wants to pause layers nobody watches.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wanted {
    pub spatial: u8,
    pub temporal: u8,
}

impl Wanted {
    pub const ALL: Self = Self {
        spatial: MAX_SPATIAL,
        temporal: MAX_TEMPORAL,
    };

    /// Only the smallest picture at the lowest frame rate.
    pub const THUMB: Self = Self {
        spatial: 0,
        temporal: 0,
    };

    pub fn admits(self, layer: Layer) -> bool {
        layer.spatial <= self.spatial && layer.temporal <= self.temporal
    }

    /// The lower of each part.
    pub fn min(self, other: Self) -> Self {
        Self {
            spatial: self.spatial.min(other.spatial),
            temporal: self.temporal.min(other.temporal),
        }
    }

    /// The higher of each part.
    pub fn max(self, other: Self) -> Self {
        Self {
            spatial: self.spatial.max(other.spatial),
            temporal: self.temporal.max(other.temporal),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_valid_layer_round_trips() {
        for spatial in 0..=MAX_SPATIAL {
            for temporal in 0..=MAX_TEMPORAL {
                for keyframe in [false, true] {
                    let layer = Layer::new(spatial, temporal, keyframe).unwrap();
                    assert_eq!(Layer::decode(layer.encode()).unwrap(), layer);
                }
            }
        }
    }

    #[test]
    fn out_of_range_and_unknown_flags_are_refused() {
        assert!(Layer::new(MAX_SPATIAL + 1, 0, false).is_err());
        assert!(Layer::new(0, MAX_TEMPORAL + 1, false).is_err());
        assert!(Layer::decode([0b10, 0]).is_err());
        assert!(Layer::decode([0, 0x30]).is_err());
        assert!(Layer::decode([0, 0x04]).is_err());
    }
}
