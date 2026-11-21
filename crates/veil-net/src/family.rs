//! Address families: dual-stack sockets, this machine's public IPv6 address, and IPv6-first
//! dialling.
//!
//! Purpose: most home connections now have IPv6 as well as IPv4. IPv6 has no NAT, only the
//! router's stateful firewall, so a simultaneous dial gets through almost every home router
//! and carrier-grade NAT no longer matters. This module lets a node use both families on one
//! socket and prefer IPv6 when both ends have it.
//!
//! - [`dual_stack`]: one UDP socket on `[::]` that also carries IPv4 (as IPv4-mapped
//!   addresses). One socket matters: hole punching needs the same socket to listen and dial.
//! - [`global_ipv6`]: the public IPv6 address this machine sends from, if it has one.
//! - [`own_candidates`]: the addresses a node tells a rendezvous it can be dialled at.
//! - [`dial_order`]: which candidates to dial and when. IPv6 first; IPv4 after a short head
//!   start ([`V4_HEAD_START`], as RFC 8305 "Happy Eyeballs" suggests), so an IPv6 path that
//!   works wins, and one that does not costs a quarter of a second.
//!
//! Invariants:
//! - Finding the IPv6 address sends nothing: connecting a UDP socket only asks the routing
//!   table which source address would be used.
//! - Only global unicast IPv6 addresses are given out ([`is_global_v6`]): never link-local,
//!   unique local (ULA), loopback, documentation, Teredo or 6to4 addresses.
//! - IPv4-mapped addresses are dialled as plain IPv4, so one peer is never dialled twice.
//!
//! shortcut: [`global_ipv6`] finds one address, the one the routing table picks for outgoing
//! traffic. On Windows and macOS that is usually a temporary (privacy) address. It accepts
//! calls for days, longer than any call, so it is fine; listing every address needs native
//! calls or a new dependency. Upgrade if a machine with several public prefixes needs them.

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

/// How long IPv4 candidates wait while IPv6 candidates are tried first.
pub const V4_HEAD_START: Duration = Duration::from_millis(250);

/// A documentation address (RFC 3849): nothing is sent to it, the route is only looked up.
const ROUTE_PROBE: Ipv6Addr = Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1);

/// A UDP socket on `[::]:port` that carries IPv4 as well. Fails if the system has no IPv6 or
/// will not make the socket dual-stack, so the caller can fall back to IPv4.
pub fn dual_stack(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    // Windows and some BSDs default to IPv6 only.
    socket.set_only_v6(false)?;
    socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
    Ok(socket.into())
}

/// Whether `ip` is a public IPv6 address another network can dial: global unicast
/// (`2000::/3`), but not documentation (`2001:db8::/32`, `3fff::/20`), Teredo (`2001::/32`)
/// or 6to4 (`2002::/16`), which are tunnels or examples rather than a home's own address.
pub fn is_global_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let global_unicast = s[0] & 0xe000 == 0x2000;
    let documentation = (s[0] == 0x2001 && s[1] == 0x0db8) || s[0] & 0xfff0 == 0x3ff0;
    let teredo = s[0] == 0x2001 && s[1] == 0;
    let six_to_four = s[0] == 0x2002;
    global_unicast && !documentation && !teredo && !six_to_four
}

/// The public IPv6 address this machine sends from, if it has one.
pub fn global_ipv6() -> Option<Ipv6Addr> {
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((ROUTE_PROBE, 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V6(ip) if is_global_v6(ip) => Some(ip),
        _ => None,
    }
}

/// The addresses a node bound at `local` can be dialled at, beyond the one a rendezvous sees:
/// a specific address as it is; for a dual-stack socket (`[::]`), this machine's public IPv6
/// address at the same port; nothing for `0.0.0.0`.
pub fn own_candidates(local: SocketAddr) -> Vec<SocketAddr> {
    own_candidates_with(local, global_ipv6)
}

fn own_candidates_with(
    local: SocketAddr,
    v6: impl FnOnce() -> Option<Ipv6Addr>,
) -> Vec<SocketAddr> {
    match local.ip() {
        IpAddr::V6(ip) if ip.is_unspecified() => v6()
            .map(|ip| vec![SocketAddr::new(ip.into(), local.port())])
            .unwrap_or_default(),
        ip if ip.is_unspecified() => Vec::new(),
        _ => vec![local],
    }
}

/// The candidates to dial and how long each waits before its dial starts. IPv4-mapped
/// addresses become plain IPv4 and duplicates are dropped. IPv6 comes first; IPv4 waits
/// [`V4_HEAD_START`] when there is any IPv6 candidate. `ipv6` says whether this socket can
/// reach IPv6 at all; without it IPv6 candidates are left out. Link-local IPv6 addresses
/// require a nonzero interface scope. At most `max` are kept.
pub fn dial_order(addrs: &[SocketAddr], ipv6: bool, max: usize) -> Vec<(SocketAddr, Duration)> {
    let mut v6 = Vec::new();
    let mut v4 = Vec::new();
    for addr in addrs {
        let addr = match addr {
            SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_none() => *addr,
            _ => SocketAddr::new(addr.ip().to_canonical(), addr.port()),
        };
        if v6.contains(&addr) || v4.contains(&addr) {
            continue;
        }
        match addr.ip() {
            IpAddr::V4(_) => v4.push(addr),
            IpAddr::V6(ip)
                if ipv6
                    && (!ip.is_unicast_link_local()
                        || matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0)) =>
            {
                v6.push(addr)
            }
            IpAddr::V6(_) => {}
        }
    }
    let wait = if v6.is_empty() {
        Duration::ZERO
    } else {
        V4_HEAD_START
    };
    v6.into_iter()
        .map(|a| (a, Duration::ZERO))
        .chain(v4.into_iter().map(|a| (a, wait)))
        .take(max)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v6(text: &str) -> Ipv6Addr {
        text.parse().unwrap()
    }

    fn sa(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn scoped_ipv6_candidates_keep_the_local_interface_index() {
        let scoped = sa("[fe80::1%7]:7710");
        assert_eq!(
            dial_order(&[scoped], true, 16),
            vec![(scoped, Duration::ZERO)]
        );
        assert!(dial_order(&[sa("[fe80::1]:7710")], true, 16).is_empty());
    }

    #[test]
    fn only_public_ipv6_addresses_are_global() {
        for public in ["2a00:23c5:1234:5600::1", "2001:470::1", "2a02:c7f::9"] {
            assert!(is_global_v6(v6(public)), "{public}");
        }
        for not_public in [
            "::1",
            "::",
            "fe80::1",
            "fd12:3456::1",
            "fc00::1",
            "ff02::1",
            "2001:db8::1",
            "3fff::1",
            "2001:0:4136:e378::1",
            "2002:c000:0204::1",
            "::ffff:203.0.113.7",
        ] {
            assert!(!is_global_v6(v6(not_public)), "{not_public}");
        }
    }

    #[test]
    fn a_dual_stack_socket_offers_its_public_ipv6_address() {
        let public = v6("2a00:23c5:1234:5600::1");
        assert_eq!(
            own_candidates_with(sa("[::]:7710"), || Some(public)),
            vec![SocketAddr::new(public.into(), 7710)]
        );
        assert!(own_candidates_with(sa("[::]:7710"), || None).is_empty());
        assert!(own_candidates_with(sa("0.0.0.0:7710"), || Some(public)).is_empty());
        assert_eq!(
            own_candidates_with(sa("192.168.1.20:7710"), || Some(public)),
            vec![sa("192.168.1.20:7710")]
        );
    }

    #[test]
    fn ipv6_goes_first_and_ipv4_waits_its_head_start() {
        let addrs = [
            sa("203.0.113.7:4433"),
            sa("[2a00:23c5::1]:4433"),
            sa("192.168.1.20:4433"),
            sa("[2a02:c7f::9]:4433"),
        ];
        assert_eq!(
            dial_order(&addrs, true, 16),
            vec![
                (sa("[2a00:23c5::1]:4433"), Duration::ZERO),
                (sa("[2a02:c7f::9]:4433"), Duration::ZERO),
                (sa("203.0.113.7:4433"), V4_HEAD_START),
                (sa("192.168.1.20:4433"), V4_HEAD_START),
            ]
        );
    }

    #[test]
    fn ipv4_alone_starts_at_once() {
        let addrs = [sa("203.0.113.7:4433"), sa("127.0.0.1:4433")];
        assert_eq!(
            dial_order(&addrs, true, 16),
            vec![
                (sa("203.0.113.7:4433"), Duration::ZERO),
                (sa("127.0.0.1:4433"), Duration::ZERO),
            ]
        );
    }

    #[test]
    fn an_ipv4_socket_skips_ipv6_and_does_not_wait() {
        let addrs = [sa("[2a00:23c5::1]:4433"), sa("203.0.113.7:4433")];
        assert_eq!(
            dial_order(&addrs, false, 16),
            vec![(sa("203.0.113.7:4433"), Duration::ZERO)]
        );
    }

    #[test]
    fn mapped_duplicates_and_link_local_are_dropped() {
        let addrs = [
            sa("[::ffff:203.0.113.7]:4433"),
            sa("203.0.113.7:4433"),
            sa("[fe80::1]:4433"),
            sa("[2a00:23c5::1]:4433"),
            sa("[2a00:23c5::1]:4433"),
        ];
        assert_eq!(
            dial_order(&addrs, true, 16),
            vec![
                (sa("[2a00:23c5::1]:4433"), Duration::ZERO),
                (sa("203.0.113.7:4433"), V4_HEAD_START),
            ]
        );
        assert_eq!(dial_order(&addrs, true, 1).len(), 1);
    }
}
