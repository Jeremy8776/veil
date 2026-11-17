//! Which IPv4 addresses are on the public internet, and which are on a local network.
//!
//! Purpose: a router's answer is only useful when its external address is public. A private
//! or shared (RFC 6598, `100.64.0.0/10`) external address means a second NAT sits between the
//! router and the internet, usually the provider's carrier-grade NAT, and a mapping there
//! opens nothing guests can reach. Answers from the network must also come from a local
//! address before Veil talks to them.

use std::net::Ipv4Addr;

/// True for an address anyone on the internet can send to.
pub fn is_public(ip: Ipv4Addr) -> bool {
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || is_shared(ip)
        || ip.octets()[0] == 0
        || ip.octets()[0] >= 240)
}

/// The carrier-grade NAT range, `100.64.0.0/10` (RFC 6598).
pub fn is_shared(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

/// True for an address a home router may have on the local network: private, shared or
/// link-local. Routers that answer UPnP from anywhere else are ignored.
pub fn is_local(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local() || is_shared(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> Ipv4Addr {
        text.parse().unwrap_or(Ipv4Addr::UNSPECIFIED)
    }

    #[test]
    fn public_and_not() {
        for good in ["8.8.8.8", "81.2.69.160", "100.63.255.255", "100.128.0.1"] {
            assert!(is_public(ip(good)), "{good}");
        }
        for bad in [
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "169.254.1.1",
            "127.0.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
        ] {
            assert!(!is_public(ip(bad)), "{bad}");
        }
    }

    #[test]
    fn local_routers() {
        assert!(is_local(ip("192.168.0.1")));
        assert!(is_local(ip("100.64.0.1")));
        assert!(!is_local(ip("8.8.8.8")));
    }
}
