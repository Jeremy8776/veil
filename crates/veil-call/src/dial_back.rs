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
//! - Only [`punch_targets`] are dialled: addresses that name one host and a port. The helper's
//!   reflection is not proof that the peer owns an address, but a punch is how two people
//!   behind routers reach each other the first time, so it is trusted for that one bounded
//!   dial. Key pinning refuses anyone but the named peer at the address.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
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

/// Keeps the addresses in a punch plan worth one handshake: a single host and a real port,
/// at most [`MAX_NOTICE_ADDRS`]. Unspecified, multicast and broadcast addresses are dropped.
pub(crate) fn punch_targets(addrs: &mut Vec<SocketAddr>) {
    addrs.retain(|addr| {
        let ip = addr.ip();
        let one_host = match ip {
            IpAddr::V4(v4) => !v4.is_broadcast(),
            IpAddr::V6(_) => true,
        };
        addr.port() != 0 && one_host && !ip.is_unspecified() && !ip.is_multicast()
    });
    addrs.truncate(MAX_NOTICE_ADDRS);
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
    fn punch_targets_keep_single_hosts_and_stay_bounded() {
        // A typo here parses to port 0, is dropped, and fails the comparison below.
        let parse = |a: &str| a.parse().unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
        let mut addrs: Vec<SocketAddr> = [
            "203.0.113.7:4433",
            "0.0.0.0:4433",
            "255.255.255.255:4433",
            "224.0.0.1:4433",
            "[ff02::1]:4433",
            "[::]:4433",
            "198.51.100.2:0",
            "[2001:db8::1]:4433",
        ]
        .into_iter()
        .map(parse)
        .collect();
        punch_targets(&mut addrs);
        assert_eq!(addrs, vec![parse("203.0.113.7:4433"), parse("[2001:db8::1]:4433")]);

        let mut many = vec![parse("203.0.113.7:4433"); MAX_NOTICE_ADDRS + 3];
        punch_targets(&mut many);
        assert_eq!(many.len(), MAX_NOTICE_ADDRS);
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
