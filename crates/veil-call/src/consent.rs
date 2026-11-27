//! Whose rendezvous requests this participant accepts, and which `Punch` notices it obeys.
//!
//! - The host takes anyone: joiners dial its address, and the lobby decides who gets in.
//! - A member takes only peers its current routes expect.
//! - A relay-only node never accepts an introduction, so its address never leaves the
//!   rendezvous.
//! - A dial-back needs an introduction agreed a moment ago, within [`DialBacks`] limits.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use veil_rendezvous::Ask;
use veil_security::PublicId;

use crate::dial_back::DialBacks;

/// Whose requests this participant accepts. Shared with its `CallNode` and media session.
#[derive(Default)]
pub(crate) struct Consent {
    /// The host: anyone holding the invite may be introduced or relayed to it. Set when this
    /// participant becomes host by a handover.
    pub open: AtomicBool,
    pub relay_only: AtomicBool,
    /// Peers the current routes link to.
    pub expected: Mutex<HashSet<PublicId>>,
    /// Introductions agreed to, and the dial-backs they led to.
    pub dial_backs: Mutex<DialBacks>,
}

impl Consent {
    pub fn allows(&self, from: &PublicId, kind: Ask) -> bool {
        if kind == Ask::Introduce && self.relay_only.load(Ordering::Relaxed) {
            return false;
        }
        self.open.load(Ordering::Relaxed) || self.expected.lock().is_ok_and(|e| e.contains(from))
    }

    /// Whether to accept this request. Accepting an introduction allows one dial-back.
    pub fn agree(&self, from: &PublicId, kind: Ask) -> bool {
        let allowed = self.allows(from, kind);
        if allowed
            && kind == Ask::Introduce
            && let Ok(mut backs) = self.dial_backs.lock()
        {
            backs.agree(*from, Instant::now());
        }
        allowed
    }

    /// Whether a `Punch` notice for `peer`, naming `addrs` addresses, should be dialled.
    pub fn may_dial_back(&self, peer: &PublicId, addrs: usize) -> bool {
        self.allows(peer, Ask::Introduce)
            && self
                .dial_backs
                .lock()
                .is_ok_and(|mut backs| backs.allow(*peer, addrs, Instant::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_follows_role_routes_and_relay_only() {
        let (known, stranger) = (PublicId([1; 32]), PublicId([2; 32]));
        let member = Consent::default();
        assert!(
            !member.allows(&known, Ask::Introduce),
            "nobody before routes"
        );
        member.expected.lock().map(|mut e| e.insert(known)).ok();
        assert!(member.allows(&known, Ask::Introduce));
        assert!(member.allows(&known, Ask::Relay));
        assert!(
            !member.allows(&stranger, Ask::Introduce),
            "an invite alone is not enough"
        );
        assert!(!member.allows(&stranger, Ask::Relay));
        member.relay_only.store(true, Ordering::Relaxed);
        assert!(
            !member.allows(&known, Ask::Introduce),
            "relay-only hides its address"
        );
        assert!(
            member.allows(&known, Ask::Relay),
            "but still takes relayed links"
        );

        let host = Consent {
            open: AtomicBool::new(true),
            ..Consent::default()
        };
        assert!(
            host.allows(&stranger, Ask::Introduce),
            "joiners dial the host"
        );
        host.relay_only.store(true, Ordering::Relaxed);
        assert!(!host.allows(&stranger, Ask::Introduce));
        assert!(host.allows(&stranger, Ask::Relay));
    }

    #[test]
    fn only_an_agreed_introduction_allows_a_dial_back() {
        let peer = PublicId([1; 32]);
        let host = Consent {
            open: AtomicBool::new(true),
            ..Consent::default()
        };
        assert!(!host.may_dial_back(&peer, 1), "nothing was agreed");
        assert!(host.agree(&peer, Ask::Relay));
        assert!(
            !host.may_dial_back(&peer, 1),
            "a relay is not an introduction"
        );
        assert!(host.agree(&peer, Ask::Introduce));
        assert!(host.may_dial_back(&peer, 1));

        // Agreed, then this node went relay-only before the notice came.
        let other = PublicId([2; 32]);
        assert!(host.agree(&other, Ask::Introduce));
        host.relay_only.store(true, Ordering::Relaxed);
        assert!(!host.may_dial_back(&other, 1));
        assert!(
            !host.agree(&other, Ask::Introduce),
            "and it agrees to no more"
        );
    }
}
