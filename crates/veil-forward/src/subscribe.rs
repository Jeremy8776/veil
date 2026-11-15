//! Layer subscriptions: which SVC layers of each sender a receiver wants from the forwarder.
//!
//! Purpose: in a 16-person grid most tiles are thumbnails, so a receiver asks for the base
//! spatial layer of most senders and a forwarder drops the rest, reading only the plaintext
//! [`Layer`] marker on each sealed frame.
//!
//! Invariants:
//! - A receiver gets every layer of a sender until it subscribes otherwise.
//! - Lowering takes effect at once. Raising waits for the sender's next keyframe, because a
//!   higher layer cannot decode from the middle of a group of pictures. Until then the
//!   receiver keeps the layers it had.
//! - Audio is never filtered: it is sealed as the base layer.
//!
//! shortcut: a receiver that raises waits for the sender's periodic keyframe; nobody asks the
//! sender for one. Add an end-to-end keyframe request when `veil-media` encodes.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use veil_security::layer::{MAX_SPATIAL, MAX_TEMPORAL};
use veil_security::{Layer, PublicId};

/// Most senders one subscription message may name. Larger than any room.
pub const MAX_SUBSCRIPTIONS: usize = 256;

/// The highest layers a receiver wants from one sender.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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

    fn min(self, other: Self) -> Self {
        Self {
            spatial: self.spatial.min(other.spatial),
            temporal: self.temporal.min(other.temporal),
        }
    }
}

/// One receiver's filter for one sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gate {
    current: Wanted,
    /// A raise waiting for a keyframe.
    pending: Option<Wanted>,
}

impl Gate {
    const OPEN: Self = Self {
        current: Wanted::ALL,
        pending: None,
    };

    fn set(&mut self, wanted: Wanted) {
        let lowered = self.current.min(wanted);
        self.pending = (lowered != wanted).then_some(wanted);
        self.current = lowered;
    }

    fn pass(&mut self, layer: Layer) -> bool {
        if let Some(pending) = self.pending
            && layer.keyframe
            && pending.admits(layer)
        {
            self.current = pending;
            self.pending = None;
        }
        self.current.admits(layer)
    }
}

/// Everything one receiver has asked for.
#[derive(Debug, Default)]
pub(crate) struct Subscriptions {
    gates: HashMap<PublicId, Gate>,
}

impl Subscriptions {
    /// Applies a receiver's request. Entries past [`MAX_SUBSCRIPTIONS`] senders are ignored,
    /// so a member cannot grow the forwarder's memory without bound.
    pub(crate) fn update(&mut self, request: &[(PublicId, Wanted)]) {
        for (sender, wanted) in request {
            let wanted = wanted.min(Wanted::ALL);
            if let Some(gate) = self.gates.get_mut(sender) {
                gate.set(wanted);
            } else if self.gates.len() < MAX_SUBSCRIPTIONS {
                let mut gate = Gate::OPEN;
                gate.set(wanted);
                self.gates.insert(*sender, gate);
            }
        }
    }

    /// Whether this receiver gets `sender`'s frame in `layer`.
    pub(crate) fn pass(&mut self, sender: &PublicId, layer: Layer) -> bool {
        self.gates.get_mut(sender).is_none_or(|gate| gate.pass(layer))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const ALICE: PublicId = PublicId([1; 32]);
    const BOB: PublicId = PublicId([2; 32]);

    fn layer(spatial: u8, temporal: u8, keyframe: bool) -> Layer {
        Layer::new(spatial, temporal, keyframe).unwrap()
    }

    #[test]
    fn everything_passes_until_a_receiver_subscribes() {
        let mut subs = Subscriptions::default();
        assert!(subs.pass(&ALICE, layer(2, 3, false)));
        assert!(subs.pass(&ALICE, Layer::BASE));
    }

    #[test]
    fn lowering_drops_higher_layers_at_once_and_only_for_that_sender() {
        let mut subs = Subscriptions::default();
        subs.update(&[(ALICE, Wanted::THUMB)]);
        assert!(subs.pass(&ALICE, Layer::BASE));
        assert!(!subs.pass(&ALICE, layer(1, 0, false)));
        assert!(!subs.pass(&ALICE, layer(0, 1, false)));
        assert!(subs.pass(&BOB, layer(2, 3, false)));
    }

    #[test]
    fn raising_waits_for_a_keyframe() {
        let mut subs = Subscriptions::default();
        subs.update(&[(ALICE, Wanted::THUMB)]);
        subs.update(&[(ALICE, Wanted::ALL)]);
        assert!(!subs.pass(&ALICE, layer(2, 0, false)), "mid group of pictures");
        assert!(subs.pass(&ALICE, layer(0, 0, true)), "key picture, base layer");
        assert!(subs.pass(&ALICE, layer(2, 0, true)));
        assert!(subs.pass(&ALICE, layer(2, 3, false)));
    }

    #[test]
    fn a_mixed_change_lowers_now_and_raises_at_the_keyframe() {
        let mut subs = Subscriptions::default();
        subs.update(&[(ALICE, Wanted { spatial: 0, temporal: 3 })]);
        subs.update(&[(ALICE, Wanted { spatial: 2, temporal: 0 })]);
        assert!(!subs.pass(&ALICE, layer(0, 1, false)), "temporal lowered at once");
        assert!(!subs.pass(&ALICE, layer(1, 0, false)), "spatial raise not yet");
        assert!(subs.pass(&ALICE, layer(0, 0, true)));
        assert!(subs.pass(&ALICE, layer(1, 0, false)));
        assert!(!subs.pass(&ALICE, layer(1, 1, false)));
    }

    #[test]
    fn out_of_range_requests_are_clamped() {
        let mut subs = Subscriptions::default();
        subs.update(&[(ALICE, Wanted { spatial: 200, temporal: 200 })]);
        assert!(subs.pass(&ALICE, layer(2, 3, false)));
        assert_eq!(subs.gates[&ALICE].pending, None);
    }

    #[test]
    fn subscriptions_are_bounded() {
        let mut subs = Subscriptions::default();
        let many: Vec<_> = (0..=MAX_SUBSCRIPTIONS)
            .map(|i| {
                let mut key = [0u8; 32];
                key[..8].copy_from_slice(&(i as u64).to_be_bytes());
                (PublicId(key), Wanted::THUMB)
            })
            .collect();
        subs.update(&many);
        assert_eq!(subs.gates.len(), MAX_SUBSCRIPTIONS);
        assert!(subs.pass(&many[MAX_SUBSCRIPTIONS].0, layer(2, 0, false)), "ignored entry stays open");
    }
}
