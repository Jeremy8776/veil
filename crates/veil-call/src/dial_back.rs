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
//! - Only [`punch_targets`] are dialled: addresses that name one host and a port, judged in
//!   their IPv4 form when IPv4-mapped. The helper's reflection is not proof that the peer owns
//!   an address, but a punch is how two people behind routers reach each other the first time,
//!   so it is trusted for that one bounded dial. Key pinning refuses anyone but the named peer.
//! - A punch never aims nearer this device than the helper is ([`Reach`]): a helper on the
//!   internet cannot point it at this device's loopback or its local network, where a helper
//!   on the local network would genuinely see local addresses.

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

/// How near this device an address is. Ordered: a helper at one reach may name addresses at
/// that reach or further out, never nearer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reach {
    Internet,
    /// Private, link-local and shared (100.64.0.0/10) IPv4, unique local and link-local IPv6.
    /// Shared space holds carrier NAT and overlay VPNs such as Tailscale: not the internet,
    /// though `veil_net::local::is_local_scope` rightly does not count it as shareable.
    LocalNetwork,
    Loopback,
}

impl Reach {
    pub fn of(ip: IpAddr) -> Self {
        let ip = ip.to_canonical();
        if ip.is_loopback() {
            Self::Loopback
        } else if veil_net::local::is_local_scope(ip) || is_shared(ip) {
            Self::LocalNetwork
        } else {
            Self::Internet
        }
    }

    /// The nearest reach among the helper's own addresses.
    pub fn of_helper(addrs: &[SocketAddr]) -> Self {
        addrs
            .iter()
            .map(|a| Self::of(a.ip()))
            .max()
            .unwrap_or(Self::Internet)
    }
}

/// 100.64.0.0/10 (RFC 6598). `Ipv4Addr::is_shared` is not stable yet.
fn is_shared(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64)
}

/// Keeps the addresses in a punch plan worth one handshake: a single host and a real port, no
/// nearer than `helper`, at most [`MAX_NOTICE_ADDRS`]. Unspecified, multicast and broadcast
/// addresses are dropped, including IPv4-mapped ones.
pub(crate) fn punch_targets(addrs: &mut Vec<SocketAddr>, helper: Reach) {
    addrs.retain(|addr| {
        let ip = addr.ip().to_canonical();
        let one_host = match ip {
            IpAddr::V4(v4) => !v4.is_broadcast(),
            IpAddr::V6(_) => true,
        };
        addr.port() != 0
            && one_host
            && !ip.is_unspecified()
            && !ip.is_multicast()
            && Reach::of(ip) <= helper
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
            "[::ffff:224.0.0.251]:5353",
            "[::ffff:255.255.255.255]:4433",
            "[::ffff:0.0.0.0]:4433",
            "[2001:db8::1]:4433",
        ]
        .into_iter()
        .map(parse)
        .collect();
        punch_targets(&mut addrs, Reach::Loopback);
        assert_eq!(addrs, vec![parse("203.0.113.7:4433"), parse("[2001:db8::1]:4433")]);

        let mut many = vec![parse("203.0.113.7:4433"); MAX_NOTICE_ADDRS + 3];
        punch_targets(&mut many, Reach::Internet);
        assert_eq!(many.len(), MAX_NOTICE_ADDRS);
    }

    #[test]
    fn a_punch_never_aims_nearer_than_the_helper() {
        let parse = |a: &str| a.parse().unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
        let all = [
            "203.0.113.7:4433",
            "192.168.1.1:80",
            "[fe80::1]:4433",
            "[::ffff:10.0.0.1]:4433",
            "100.101.102.103:4433",
            "127.0.0.1:4433",
            "[::1]:4433",
            "[::ffff:127.0.0.1]:4433",
        ]
        .map(parse);
        let kept = |helper| {
            let mut addrs = all.to_vec();
            punch_targets(&mut addrs, helper);
            addrs.len()
        };
        assert_eq!(kept(Reach::Internet), 1, "a public helper names public addresses only");
        assert_eq!(kept(Reach::LocalNetwork), 5, "a LAN helper may name LAN addresses");
        assert_eq!(kept(Reach::Loopback), 8);
        assert!(!is_shared("100.63.255.255".parse().unwrap_or(IpAddr::from([100, 64, 0, 0]))));
        assert!(!is_shared("100.128.0.0".parse().unwrap_or(IpAddr::from([100, 64, 0, 0]))));

        let helper = |a: &[&str]| Reach::of_helper(&a.iter().map(|a| parse(a)).collect::<Vec<_>>());
        assert_eq!(helper(&["203.0.113.9:443"]), Reach::Internet);
        assert_eq!(helper(&["203.0.113.9:443", "192.168.1.5:443"]), Reach::LocalNetwork);
        assert_eq!(helper(&["127.0.0.1:443"]), Reach::Loopback);
        assert_eq!(helper(&[]), Reach::Internet);
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
