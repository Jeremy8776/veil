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
//!   receiver keeps the layers it had. The first keyframe request can be lost (it goes on a
//!   bounded control queue), so while a raise waits the forwarder asks the sender again every
//!   [`ASK_AGAIN`]; the sender's periodic keyframe is 30 s away.
//! - Audio is never filtered: it is sealed as the base layer.
//!
//! shortcut: every spatial layer up to the wanted one is forwarded. That suits SVC, where each
//! layer needs the one below, but a simulcast receiver decodes only the top one and the lower
//! ones waste its download. Add a simulcast flag when a platform encoder sends simulcast.

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub use veil_security::layer::Wanted;
use veil_security::{Layer, PublicId};

/// Most senders one subscription message may name. Larger than any room.
pub const MAX_SUBSCRIPTIONS: usize = 256;
/// How long a raise waits for its keyframe before the forwarder asks the sender again.
pub const ASK_AGAIN: Duration = Duration::from_millis(1_500);

/// One receiver's filter for one sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gate {
    current: Wanted,
    /// A raise waiting for a keyframe.
    pending: Option<Wanted>,
    /// When a keyframe was last asked for this raise, once a frame has shown it still waits.
    asked: Option<Instant>,
}

impl Gate {
    const OPEN: Self = Self {
        current: Wanted::ALL,
        pending: None,
        asked: None,
    };

    fn set(&mut self, wanted: Wanted) {
        let lowered = self.current.min(wanted);
        self.pending = (lowered != wanted).then_some(wanted);
        self.current = lowered;
        self.asked = None;
    }

    /// Whether to forward `layer`, and whether to ask the sender for a keyframe again.
    fn pass(&mut self, layer: Layer, now: Instant) -> (bool, bool) {
        if let Some(pending) = self.pending
            && layer.keyframe
            && pending.admits(layer)
        {
            self.current = pending;
            self.pending = None;
            self.asked = None;
        }
        let mut ask = false;
        if self.pending.is_some() {
            // The first request went when the raise arrived; time from the first frame after.
            match self.asked {
                None => self.asked = Some(now),
                Some(at) if now.saturating_duration_since(at) >= ASK_AGAIN => {
                    self.asked = Some(now);
                    ask = true;
                }
                Some(_) => {}
            }
        }
        (self.current.admits(layer), ask)
    }
}

/// Everything one receiver has asked for.
#[derive(Debug, Default)]
pub(crate) struct Subscriptions {
    gates: HashMap<PublicId, Gate>,
}

impl Subscriptions {
    /// Applies a receiver's request. Entries past [`MAX_SUBSCRIPTIONS`] senders are ignored,
    /// so a member cannot grow the forwarder's memory without bound. Returns the senders
    /// whose raise now waits for a keyframe, so the forwarder can ask them for one.
    pub(crate) fn update(&mut self, request: &[(PublicId, Wanted)]) -> Vec<PublicId> {
        let mut raised = Vec::new();
        for (sender, wanted) in request {
            let wanted = wanted.min(Wanted::ALL);
            let room = self.gates.len() < MAX_SUBSCRIPTIONS;
            let gate = match self.gates.get_mut(sender) {
                Some(gate) => gate,
                None if room => self.gates.entry(*sender).or_insert(Gate::OPEN),
                None => continue,
            };
            gate.set(wanted);
            if gate.pending.is_some() && !raised.contains(sender) {
                raised.push(*sender);
            }
        }
        raised
    }

    /// Whether this receiver gets `sender`'s frame in `layer`, and whether `sender` should be
    /// asked again for the keyframe a raise still waits for.
    pub(crate) fn pass_at(&mut self, sender: &PublicId, layer: Layer, now: Instant) -> (bool, bool) {
        self.gates
            .get_mut(sender)
            .map_or((true, false), |gate| gate.pass(layer, now))
    }

    #[cfg(test)]
    fn pass(&mut self, sender: &PublicId, layer: Layer) -> bool {
        self.pass_at(sender, layer, Instant::now()).0
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
        assert!(subs.update(&[(ALICE, Wanted::THUMB)]).is_empty(), "lowering needs no keyframe");
        assert_eq!(subs.update(&[(ALICE, Wanted::ALL)]), [ALICE]);
        assert!(!subs.pass(&ALICE, layer(2, 0, false)), "mid group of pictures");
        assert!(subs.pass(&ALICE, layer(0, 0, true)), "key picture, base layer");
        assert!(subs.pass(&ALICE, layer(2, 0, true)));
        assert!(subs.pass(&ALICE, layer(2, 3, false)));
    }

    #[test]
    fn a_raise_that_waits_asks_again() {
        let mut subs = Subscriptions::default();
        subs.update(&[(ALICE, Wanted::THUMB)]);
        assert_eq!(subs.update(&[(ALICE, Wanted::ALL)]), [ALICE], "first ask");
        let t0 = Instant::now();
        assert_eq!(subs.pass_at(&ALICE, Layer::BASE, t0), (true, false), "the clock starts");
        let soon = t0 + ASK_AGAIN - Duration::from_millis(1);
        assert_eq!(subs.pass_at(&ALICE, Layer::BASE, soon), (true, false));
        let later = t0 + ASK_AGAIN;
        assert_eq!(subs.pass_at(&ALICE, Layer::BASE, later), (true, true), "first ask lost");
        assert_eq!(subs.pass_at(&ALICE, Layer::BASE, later), (true, false), "once per interval");
        assert_eq!(subs.pass_at(&ALICE, layer(2, 0, true), later), (true, false), "raised");
        let much_later = later + ASK_AGAIN * 4;
        assert_eq!(subs.pass_at(&ALICE, layer(2, 3, false), much_later), (true, false));
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
