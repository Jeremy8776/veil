//! The receiver's check before a layer goes to the decoder.
//!
//! Purpose: a decoder given a layer whose reference it never decoded shows corrupt pictures
//! until the next keyframe. [`Gate::admit`] keeps one chain per spatial layer for one sender
//! and passes a layer only if its reference (and, for SVC, the layer below) was decoded.
//!
//! Invariants:
//! - A keyframe restarts its spatial layer's chain.
//! - Pictures on one spatial layer go to the decoder in increasing order. An older or
//!   repeated picture is dropped.
//! - A keyframe is requested only when a temporal layer 0 picture cannot decode. Loss in a
//!   higher temporal layer heals at the next layer 0 picture, so it never asks.
//! - One request per break: the gate asks again only after a keyframe has arrived.
//!
//! shortcut: a layer that arrives before its reference is dropped, not held back for
//! reordering. Frames travel on separate QUIC streams, so this can drop a few pictures on a
//! reordering link; add a short reorder buffer if receivers see it.

use std::collections::VecDeque;

use veil_security::Layer;
use veil_security::layer::MAX_SPATIAL;

use crate::payload::VideoHeader;

/// Decoded pictures remembered per spatial layer. A reference is at most one temporal
/// period (8 pictures) back, so this is enough.
const REMEMBERED: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Hand the layer to the decoder.
    Decode,
    /// Drop it.
    Drop,
    /// Drop it, and ask the sender for a keyframe.
    DropAndRequestKeyframe,
}

#[derive(Default)]
struct Chain {
    decoded: VecDeque<u64>,
    asked: bool,
}

impl Chain {
    fn has(&self, picture: u64) -> bool {
        self.decoded.contains(&picture)
    }

    fn last(&self) -> Option<u64> {
        self.decoded.back().copied()
    }

    fn push(&mut self, picture: u64) {
        if self.decoded.len() == REMEMBERED {
            self.decoded.pop_front();
        }
        self.decoded.push_back(picture);
    }
}

/// One sender's decode state.
#[derive(Default)]
pub struct Gate {
    chains: [Chain; MAX_SPATIAL as usize + 1],
}

impl Gate {
    /// `layer` is the sealed frame's marker, `header` its opened video header.
    pub fn admit(&mut self, layer: Layer, header: &VideoHeader) -> Verdict {
        let spatial = usize::from(layer.spatial);
        if layer.keyframe != header.reference.is_none() || (spatial == 0 && header.needs_lower) {
            return Verdict::Drop;
        }
        let lower_ok = !header.needs_lower
            || spatial
                .checked_sub(1)
                .and_then(|below| self.chains.get(below))
                .is_some_and(|c| c.has(header.picture));
        let Some(chain) = self.chains.get_mut(spatial) else {
            return Verdict::Drop;
        };
        if chain.last().is_some_and(|last| header.picture <= last) {
            return Verdict::Drop;
        }
        let decodable = lower_ok && header.reference.is_none_or(|r| chain.has(r));
        if decodable {
            if layer.keyframe {
                chain.decoded.clear();
                chain.asked = false;
            }
            chain.push(header.picture);
            return Verdict::Decode;
        }
        if layer.temporal == 0 && !chain.asked {
            chain.asked = true;
            return Verdict::DropAndRequestKeyframe;
        }
        Verdict::Drop
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::payload::Codec;

    fn frame(spatial: u8, temporal: u8, picture: u64, reference: Option<u64>) -> (Layer, VideoHeader) {
        let layer = Layer::new(spatial, temporal, reference.is_none()).unwrap();
        let header = VideoHeader {
            codec: Codec::H264,
            picture,
            reference,
            needs_lower: false,
            timestamp_us: 0,
            width: 640,
            height: 360,
        };
        (layer, header)
    }

    fn admit(gate: &mut Gate, f: (Layer, VideoHeader)) -> Verdict {
        gate.admit(f.0, &f.1)
    }

    #[test]
    fn an_intact_stream_decodes() {
        let mut gate = Gate::default();
        assert_eq!(admit(&mut gate, frame(0, 0, 0, None)), Verdict::Decode);
        assert_eq!(admit(&mut gate, frame(0, 2, 1, Some(0))), Verdict::Decode);
        assert_eq!(admit(&mut gate, frame(0, 1, 2, Some(0))), Verdict::Decode);
        assert_eq!(admit(&mut gate, frame(0, 2, 3, Some(2))), Verdict::Decode);
        assert_eq!(admit(&mut gate, frame(0, 0, 4, Some(0))), Verdict::Decode);
    }

    #[test]
    fn higher_layer_loss_heals_without_asking() {
        let mut gate = Gate::default();
        admit(&mut gate, frame(0, 0, 0, None));
        // Picture 2 (temporal 1) is lost, so 3 cannot decode, but 4 can.
        assert_eq!(admit(&mut gate, frame(0, 2, 3, Some(2))), Verdict::Drop);
        assert_eq!(admit(&mut gate, frame(0, 0, 4, Some(0))), Verdict::Decode);
    }

    #[test]
    fn base_layer_loss_asks_once_until_a_keyframe() {
        let mut gate = Gate::default();
        admit(&mut gate, frame(0, 0, 0, None));
        // Picture 4 is lost.
        assert_eq!(admit(&mut gate, frame(0, 0, 8, Some(4))), Verdict::DropAndRequestKeyframe);
        assert_eq!(admit(&mut gate, frame(0, 0, 12, Some(8))), Verdict::Drop, "asked already");
        assert_eq!(admit(&mut gate, frame(0, 0, 13, None)), Verdict::Decode);
        assert_eq!(admit(&mut gate, frame(0, 0, 21, Some(17))), Verdict::DropAndRequestKeyframe);
    }

    #[test]
    fn joining_mid_stream_asks_for_a_keyframe() {
        let mut gate = Gate::default();
        assert_eq!(admit(&mut gate, frame(1, 2, 41, Some(40))), Verdict::Drop);
        assert_eq!(admit(&mut gate, frame(1, 0, 44, Some(40))), Verdict::DropAndRequestKeyframe);
    }

    #[test]
    fn old_and_repeated_pictures_are_dropped() {
        let mut gate = Gate::default();
        admit(&mut gate, frame(0, 0, 4, None));
        assert_eq!(admit(&mut gate, frame(0, 0, 4, None)), Verdict::Drop);
        assert_eq!(admit(&mut gate, frame(0, 0, 3, None)), Verdict::Drop);
    }

    #[test]
    fn svc_layers_need_the_layer_below() {
        let mut gate = Gate::default();
        let (layer, mut header) = frame(1, 0, 0, None);
        header.needs_lower = true;
        assert_eq!(gate.admit(layer, &header), Verdict::DropAndRequestKeyframe);
        let mut gate = Gate::default();
        admit(&mut gate, frame(0, 0, 0, None));
        assert_eq!(gate.admit(layer, &header), Verdict::Decode);
    }

    #[test]
    fn inconsistent_frames_are_dropped() {
        let mut gate = Gate::default();
        let (layer, mut header) = frame(0, 0, 0, None);
        header.reference = Some(0);
        assert_eq!(gate.admit(layer, &header), Verdict::Drop, "keyframe with a reference");
        let (layer, mut header) = frame(0, 0, 0, None);
        header.needs_lower = true;
        assert_eq!(gate.admit(layer, &header), Verdict::Drop, "nothing below spatial 0");
    }
}
