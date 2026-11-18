//! Which `Punch` notices from the rendezvous this participant acts on.
//!
//! A dial-back sends handshake packets from this device to addresses the notice names, and the
//! rendezvous and the peer that asked both influence those addresses. So a notice is obeyed
//! only as the answer to an introduction this participant agreed to a moment ago.
//!
//! Invariants:
//! - Each agreement allows one dial-back, for [`AGREED_FOR`].
//! - One dial-back per peer per [`WINDOW`], and [`MAX_IN_WINDOW`] in all. A dial lasts at most
//!   `MAX_PUNCH_DELAY` plus the connect timeout, which is [`WINDOW`], so the same limits also
//!   bound how many run at once. A joiner that loses out still has the relay.
//! - A notice naming more addresses than the rendezvous may store for a member is refused.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use veil_rendezvous::protocol::MAX_ADDRS;
use veil_security::PublicId;

/// How long an agreement waits for its notice, which may queue behind this node's own requests.
const AGREED_FOR: Duration = Duration::from_secs(10);
const WINDOW: Duration = Duration::from_secs(10);
/// Sixteen is the call's size limit.
const MAX_IN_WINDOW: usize = 16;
/// As many as a rendezvous room holds.
const MAX_AGREED: usize = 64;
/// The address the server sees for a member, and those it listed.
const MAX_NOTICE_ADDRS: usize = MAX_ADDRS + 1;

#[derive(Default)]
pub(crate) struct DialBacks {
    agreed: HashMap<PublicId, Instant>,
    /// Dial-backs started within [`WINDOW`], oldest first.
    recent: VecDeque<(PublicId, Instant)>,
}

impl DialBacks {
    /// Records that this node agreed to introduce itself to `peer`.
    pub fn agree(&mut self, peer: PublicId, now: Instant) {
        self.agreed.retain(|_, at| now.duration_since(*at) < AGREED_FOR);
        if self.agreed.len() >= MAX_AGREED && !self.agreed.contains_key(&peer) {
            let oldest = self.agreed.iter().min_by_key(|(_, at)| **at).map(|(key, _)| *key);
            if let Some(oldest) = oldest {
                self.agreed.remove(&oldest);
            }
        }
        self.agreed.insert(peer, now);
    }

    /// Whether to dial `peer` at the `addrs` addresses a notice names. Yes uses up the
    /// agreement and counts as a dial-back.
    pub fn allow(&mut self, peer: PublicId, addrs: usize, now: Instant) -> bool {
        let agreed = self.agreed.remove(&peer);
        if addrs > MAX_NOTICE_ADDRS || !agreed.is_some_and(|at| now.duration_since(at) < AGREED_FOR) {
            return false;
        }
        while self.recent.front().is_some_and(|(_, at)| now.duration_since(*at) >= WINDOW) {
            self.recent.pop_front();
        }
        if self.recent.len() >= MAX_IN_WINDOW || self.recent.iter().any(|(p, _)| *p == peer) {
            return false;
        }
        self.recent.push_back((peer, now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> PublicId {
        PublicId([n; 32])
    }

    #[test]
    fn a_notice_nobody_agreed_to_is_refused() {
        assert!(!DialBacks::default().allow(key(1), 1, Instant::now()));
    }

    #[test]
    fn an_agreement_allows_one_dial_and_goes_stale() {
        let now = Instant::now();
        let mut backs = DialBacks::default();
        backs.agree(key(1), now);
        assert!(backs.allow(key(1), 1, now));
        assert!(!backs.allow(key(1), 1, now), "used up");

        backs.agree(key(2), now);
        assert!(!backs.allow(key(2), 1, now + AGREED_FOR), "too late");
    }

    #[test]
    fn a_peer_gets_one_dial_per_window_however_often_it_asks() {
        let now = Instant::now();
        let mut backs = DialBacks::default();
        backs.agree(key(1), now);
        assert!(backs.allow(key(1), 1, now));
        backs.agree(key(1), now);
        assert!(!backs.allow(key(1), 1, now + WINDOW / 2));
        let later = now + WINDOW;
        backs.agree(key(1), later);
        assert!(backs.allow(key(1), 1, later));
    }

    #[test]
    fn all_peers_together_are_limited() {
        let now = Instant::now();
        let mut backs = DialBacks::default();
        for n in 0..MAX_IN_WINDOW {
            let peer = key(u8::try_from(n).unwrap_or(0));
            backs.agree(peer, now);
            assert!(backs.allow(peer, 1, now));
        }
        backs.agree(key(200), now);
        assert!(!backs.allow(key(200), 1, now));
    }

    #[test]
    fn a_notice_with_too_many_addresses_is_refused() {
        let now = Instant::now();
        let mut backs = DialBacks::default();
        backs.agree(key(1), now);
        assert!(!backs.allow(key(1), MAX_NOTICE_ADDRS + 1, now));
        backs.agree(key(1), now);
        assert!(backs.allow(key(1), MAX_NOTICE_ADDRS, now));
    }

    #[test]
    fn agreements_stay_bounded_and_keep_the_newest() {
        let now = Instant::now();
        let mut backs = DialBacks::default();
        for n in 0..=MAX_AGREED {
            let at = now + Duration::from_millis(u64::try_from(n).unwrap_or(0));
            backs.agree(key(u8::try_from(n).unwrap_or(0)), at);
        }
        assert_eq!(backs.agreed.len(), MAX_AGREED);
        assert!(!backs.agreed.contains_key(&key(0)), "the oldest went first");
    }
}
