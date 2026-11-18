//! The invite's relay field: one rendezvous node, reachable at one or more addresses.
//!
//! Purpose: a host that runs its own rendezvous behind a router gives it at two addresses:
//! the router's public mapping, for guests on the internet, and the LAN address, for guests on
//! the same network (many routers do not loop their public address back inside). Joiners dial
//! all of them at once and keep the first that answers.
//!
//! Format: `KEY@IP:PORT` as `veil_net::NodeAddr` prints it, then any further addresses for the
//! same key after commas: `KEY@IP:PORT,IP:PORT`. A single address is exactly a `NodeAddr`, so
//! invites from older hosts still parse.
//!
//! Invariants:
//! - Every address is pinned to the one key, so an extra address cannot point at another node.
//! - At most [`MAX_RELAY_ADDRS`] addresses; duplicates are dropped.

use std::net::SocketAddr;

use veil_net::NodeAddr;
use veil_security::PublicId;

pub const MAX_RELAY_ADDRS: usize = 4;

/// A rendezvous node's key and every address it may be reached at, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayAddrs {
    pub key: PublicId,
    pub addrs: Vec<SocketAddr>,
}

impl RelayAddrs {
    /// Parses the relay field. `None` if it is not one key and 1 to 4 addresses.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split(',');
        let first: NodeAddr = parts.next()?.parse().ok()?;
        let mut addrs = vec![first.addr];
        for part in parts {
            let addr: SocketAddr = part.trim().parse().ok()?;
            if !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
        (addrs.len() <= MAX_RELAY_ADDRS).then_some(Self {
            key: first.key,
            addrs,
        })
    }
}

impl From<NodeAddr> for RelayAddrs {
    fn from(node: NodeAddr) -> Self {
        Self {
            key: node.key,
            addrs: vec![node.addr],
        }
    }
}

impl std::fmt::Display for RelayAddrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some((first, rest)) = self.addrs.split_first() else {
            return Ok(());
        };
        write!(
            f,
            "{}",
            NodeAddr {
                key: self.key,
                addr: *first
            }
        )?;
        for addr in rest {
            write!(f, ",{addr}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_security::Identity;

    use super::*;

    fn node(addr: &str) -> NodeAddr {
        NodeAddr {
            key: Identity::generate().public(),
            addr: addr.parse().unwrap(),
        }
    }

    #[test]
    fn a_single_node_address_still_parses() {
        let one = node("192.168.1.20:5000");
        let parsed = RelayAddrs::parse(&one.to_string()).unwrap();
        assert_eq!(parsed, RelayAddrs::from(one));
        assert_eq!(parsed.to_string(), one.to_string());
    }

    #[test]
    fn extra_addresses_round_trip_under_one_key() {
        let one = node("203.0.113.9:60593");
        let relay = RelayAddrs {
            key: one.key,
            addrs: vec![one.addr, "192.168.1.20:5000".parse().unwrap()],
        };
        let text = relay.to_string();
        assert!(text.ends_with(",192.168.1.20:5000"), "{text}");
        assert_eq!(RelayAddrs::parse(&text).unwrap(), relay);
        assert!(text.len() <= veil_security::invite::MAX_ADDR_LEN);
    }

    #[test]
    fn bad_or_too_many_addresses_are_refused() {
        let one = node("203.0.113.9:1").to_string();
        assert!(RelayAddrs::parse(&format!("{one},nonsense")).is_none());
        assert!(RelayAddrs::parse("nonsense").is_none());
        let many = format!("{one},203.0.113.9:2,203.0.113.9:3,203.0.113.9:4,203.0.113.9:5");
        assert!(RelayAddrs::parse(&many).is_none());
        let dup = RelayAddrs::parse(&format!("{one},203.0.113.9:1")).unwrap();
        assert_eq!(dup.addrs.len(), 1, "duplicates are dropped");
    }
}
