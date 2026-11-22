//! Address hints: what members may say about their own addresses, and how often.
//!
//! Invariants:
//! - A member's hints never aim other members' packets at a third party: a public address is
//!   kept only on the IP the member's link comes from, and a local one only on the link's own
//!   port ([`member_hints`]).
//! - At most one update per member per [`HINT_GAP`] reaches the engine; the newest waits.
//! - Hints are connection hints, never proof that an address belongs to a peer.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use veil_security::PublicId;

use super::{Actor, HostEvent};

/// One address update per member in this window reaches the engine; a newer one waits and
/// replaces the waiting one. A member cannot make the host replan faster than this.
pub(super) const HINT_GAP: Duration = Duration::from_secs(5);
/// Most address hints one member may report.
pub const MAX_HINTS: usize = 16;

impl Actor {
    /// A member's address update, throttled to one per [`HINT_GAP`] with the newest kept.
    pub(super) fn hint(&mut self, device: PublicId, addrs: Vec<SocketAddr>) {
        let now = Instant::now();
        match self.hints.get_mut(&device) {
            Some((last, waiting)) if now.duration_since(*last) < HINT_GAP => {
                *waiting = Some(addrs);
            }
            _ => {
                self.hints.insert(device, (now, None));
                self.emit(HostEvent::Addresses { device, addrs });
            }
        }
    }

    pub(super) fn flush_hints(&mut self) {
        let now = Instant::now();
        let mut due = Vec::new();
        for (device, (last, waiting)) in &mut self.hints {
            if now.duration_since(*last) >= HINT_GAP
                && let Some(addrs) = waiting.take()
            {
                *last = now;
                due.push((*device, addrs));
            }
        }
        for (device, addrs) in due {
            self.emit(HostEvent::Addresses { device, addrs });
        }
    }
}

/// Authenticated peers still cannot grow the directory without bound or supply broadcast
/// destinations. These are connection hints, never proof that an address belongs to a peer.
pub(crate) fn checked_candidates(
    addrs: Vec<std::net::SocketAddr>,
) -> Option<Vec<std::net::SocketAddr>> {
    if addrs.len() > 16 {
        return None;
    }
    let mut result = Vec::new();
    for addr in addrs {
        let ip = addr.ip().to_canonical();
        if addr.port() == 0
            || ip.is_unspecified()
            || ip.is_multicast()
            || matches!(ip, std::net::IpAddr::V4(v4) if v4.is_broadcast())
        {
            return None;
        }
        if !result.contains(&addr) {
            result.push(addr);
        }
    }
    Some(result)
}

/// What a member may say about its own addresses. These go into every member's directory and
/// are dialled, so a member must not be able to aim other members' packets anywhere it likes:
/// - A public address must be the IP this host sees the member's link come from.
/// - A local scope address (private, link-local, unique local) must carry the port the link
///   comes from. A member binds one socket for the whole call, so its real addresses all share
///   that port, and hints are only used in Local only calls, where no NAT changes it. Without
///   this, a member could point every other member at any service on the LAN.
/// - Local scope hints on a privileged port (below 1024) are dropped, so a member that bound
///   such a port cannot aim other members at well-known services (DNS, SNMP) on the LAN.
///
/// Loopback and documentation addresses are dropped. Too many drops the whole update.
pub(crate) fn member_hints(
    addrs: Vec<SocketAddr>,
    observed: SocketAddr,
) -> Option<Vec<SocketAddr>> {
    use std::net::IpAddr;
    if addrs.len() > MAX_HINTS {
        return None;
    }
    let seen = observed.ip().to_canonical();
    let mut result = Vec::new();
    for addr in checked_candidates(addrs)? {
        let ip = addr.ip().to_canonical();
        let local_scope = veil_net::local::is_local_scope(ip);
        let documentation = match ip {
            IpAddr::V4(v4) => v4.is_documentation(),
            IpAddr::V6(v6) => v6.segments()[..2] == [0x2001, 0x0db8],
        };
        if ip.is_loopback() || documentation {
            continue;
        }
        let own_port = addr.port() == observed.port() && addr.port() >= 1024;
        if ip == seen || (local_scope && own_port) {
            result.push(addr);
        }
    }
    Some(result)
}
