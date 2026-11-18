//! How long the host's invite links work, and when the engine makes a fresh one.
//!
//! Invariants:
//! - Every link works once, whatever its lifetime. "Until the call ends" changes only how
//!   long an unused link stays valid.
//! - No link outlives the call: the lobby that accepts its token lives in the host's memory
//!   and ends with the call, whatever the expiry inside the link says.
//! - The link shown is never expired: one that runs out is replaced (`expired`).
//! - A short link is offered only if it reaches as far as the full one (`can_shorten`).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A link's lifetime by default.
const ONE_HOUR: Duration = Duration::from_secs(3_600);
/// "Until the call ends": longer than any call, since the call ending is what stops it.
const UNTIL_END: Duration = Duration::from_secs(30 * 24 * 3_600);

/// The expiry for a link made at `now_unix`.
pub fn expiry(until_end: bool, now_unix: u64) -> u64 {
    let lifetime = if until_end { UNTIL_END } else { ONE_HOUR };
    now_unix.saturating_add(lifetime.as_secs())
}

/// True when a link that expires at `expires` no longer works at `now_unix`.
pub fn expired(expires: u64, now_unix: u64) -> bool {
    now_unix >= expires
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Whether a short link would work for everyone the full link works for. A short link is
/// fetched from the rendezvous alone, so if the call can be reached from the internet but the
/// rendezvous cannot (its port did not open), only the full link reaches those guests.
pub fn can_shorten(call: &[SocketAddr], rendezvous: &[SocketAddr]) -> bool {
    let public = |addrs: &[SocketAddr]| addrs.iter().any(|a| is_public(a.ip()));
    !public(call) || public(rendezvous)
}

fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => veil_portmap::addr::is_public(ip),
        IpAddr::V6(ip) => {
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_short_link_must_reach_as_far_as_the_full_one() {
        let addr = |text: &str| -> SocketAddr { text.parse().unwrap() };
        let (lan, lan2) = (addr("192.168.1.20:7710"), addr("192.168.1.20:5000"));
        let (public, public2) = (addr("81.2.69.160:7710"), addr("81.2.69.160:5000"));
        assert!(can_shorten(&[lan], &[lan2]), "the same network either way");
        assert!(can_shorten(&[public, lan], &[public2, lan2]), "both opened");
        assert!(can_shorten(&[lan], &[public2]), "a public rendezvous");
        assert!(!can_shorten(&[public, lan], &[lan2]), "its port did not open");
        let v6 = addr("[2001:db8::1]:7710");
        assert!(!can_shorten(&[v6], &[lan2]));
        assert!(can_shorten(&[addr("[fe80::1]:7710")], &[lan2]));
    }

    #[test]
    fn a_link_lasts_an_hour_or_until_the_call_ends() {
        assert_eq!(expiry(false, 1_000), 4_600);
        assert!(expiry(true, 1_000) > 1_000 + 7 * 24 * 3_600);
        assert_eq!(expiry(true, u64::MAX), u64::MAX, "no overflow");
    }

    #[test]
    fn a_link_stops_working_at_its_expiry() {
        assert!(!expired(4_600, 4_599));
        assert!(expired(4_600, 4_600));
    }
}
