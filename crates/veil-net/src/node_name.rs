//! A node a user names in settings: `<base64url key>@<host>:<port>`, where the host is an IP
//! address or a DNS name.
//!
//! Purpose: self-hosted nodes on dynamic DNS (and a default public node) are easier to keep
//! under a name than an IP. [`NodeName::resolve`] turns the name into a [`NodeAddr`] when a
//! call starts, so the rest of Veil (invites, plans, dials) still carries literal addresses.
//!
//! Invariants:
//! - The key is pinned. A spoofed or hijacked name can only point the dial at a server that
//!   fails the handshake, so the worst it can do is stop a call, never read or join one.
//! - The text form is bounded by [`MAX_NODE_ADDR_LEN`] before it is parsed.
//! - A name is checked as a DNS host name (letters, digits and hyphens in dot-separated
//!   labels, last label not all digits) before it reaches the resolver, so a mistyped IP
//!   such as `203.0.113.999` is refused rather than looked up.
//! - [`NodeName::resolve`] never returns an unspecified address (`0.0.0.0`, `::`), and gives
//!   up after [`RESOLVE_TIMEOUT`]. [`NodeName::resolve_ipv6`] returns a public IPv6
//!   address only.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use veil_security::PublicId;

use crate::NetError;
use crate::node_addr::{MAX_NODE_ADDR_LEN, NodeAddr};

/// How long one lookup may take before the call gives up on that server.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest DNS name (RFC 1035), and longest label in one.
const MAX_NAME_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeName {
    pub key: PublicId,
    target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Addr(SocketAddr),
    Name { host: String, port: u16 },
}

impl NodeName {
    /// The host name, if this node is named rather than given by IP.
    pub fn host_name(&self) -> Option<&str> {
        match &self.target {
            Target::Addr(_) => None,
            Target::Name { host, .. } => Some(host),
        }
    }

    pub fn port(&self) -> u16 {
        match &self.target {
            Target::Addr(addr) => addr.port(),
            Target::Name { port, .. } => *port,
        }
    }

    /// The literal address, if this node is given by IP.
    pub fn addr(&self) -> Option<SocketAddr> {
        match &self.target {
            Target::Addr(addr) => Some(*addr),
            Target::Name { .. } => None,
        }
    }

    /// The address to dial now. A name is looked up each time, so a node on dynamic DNS is
    /// found at its current address. IPv4 answers come first: a public node is usually
    /// reached over IPv4, and every participant has IPv4.
    pub async fn resolve(&self) -> Result<NodeAddr, NetError> {
        let addr = usable(self.lookup().await?);
        let addr = addr.ok_or_else(|| {
            NetError::Connect(format!("{} has no address Veil can dial", self.host()))
        })?;
        Ok(NodeAddr {
            key: self.key,
            addr,
        })
    }

    /// The node's IPv6 address, if it has one: for a check that must run over IPv6 (the
    /// host's reachability check of its own IPv6 address).
    pub async fn resolve_ipv6(&self) -> Option<NodeAddr> {
        let addr = self.lookup().await.ok()?.into_iter().find(|a| match a {
            SocketAddr::V6(v6) => {
                crate::family::is_global_v6(*v6.ip()) && a.port() != 0
            }
            SocketAddr::V4(_) => false,
        })?;
        Some(NodeAddr {
            key: self.key,
            addr,
        })
    }

    async fn lookup(&self) -> Result<Vec<SocketAddr>, NetError> {
        match &self.target {
            Target::Addr(addr) => Ok(vec![*addr]),
            Target::Name { host, port } => Ok(tokio::time::timeout(
                RESOLVE_TIMEOUT,
                tokio::net::lookup_host((host.as_str(), *port)),
            )
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Connect(format!("cannot look up {host}: {e}")))?
            .collect()),
        }
    }

    fn host(&self) -> String {
        match &self.target {
            Target::Addr(addr) => addr.ip().to_string(),
            Target::Name { host, .. } => host.clone(),
        }
    }
}

/// The first dialable address, IPv4 before IPv6.
fn usable(addrs: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let addrs: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| !a.ip().is_unspecified() && a.port() != 0)
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
}

impl From<NodeAddr> for NodeName {
    fn from(node: NodeAddr) -> Self {
        Self {
            key: node.key,
            target: Target::Addr(node.addr),
        }
    }
}

impl fmt::Display for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = URL_SAFE_NO_PAD.encode(self.key.0);
        match &self.target {
            Target::Addr(addr) => write!(f, "{key}@{addr}"),
            Target::Name { host, port } => write!(f, "{key}@{host}:{port}"),
        }
    }
}

impl FromStr for NodeName {
    type Err = NetError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() > MAX_NODE_ADDR_LEN {
            return Err(NetError::TooLarge);
        }
        let text = text.trim();
        // An IP form parses exactly as before.
        if let Ok(node) = text.parse::<NodeAddr>() {
            return Ok(node.into());
        }
        let (key, rest) = text
            .split_once('@')
            .ok_or(NetError::Protocol("node address needs key@address"))?;
        // Reuses NodeAddr's key checks with a placeholder address.
        let key = format!("{key}@0.0.0.0:1").parse::<NodeAddr>()?.key;
        let (host, port) = rest
            .rsplit_once(':')
            .ok_or(NetError::Protocol("node address needs host:port"))?;
        let port: u16 = port
            .parse()
            .map_err(|_| NetError::Protocol("node port is not a number"))?;
        if !is_host_name(host) {
            return Err(NetError::Protocol("node host is not an IP or a DNS name"));
        }
        Ok(Self {
            key,
            target: Target::Name {
                host: host.to_ascii_lowercase(),
                port,
            },
        })
    }
}

/// A DNS host name: dot-separated labels of letters, digits and inner hyphens, the last not
/// all digits (that would be a malformed IPv4 address).
fn is_host_name(host: &str) -> bool {
    if host.is_empty() || host.len() > MAX_NAME_LEN {
        return false;
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= MAX_LABEL_LEN
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    host.split('.').all(label_ok)
        && host
            .rsplit('.')
            .next()
            .is_some_and(|last| !last.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key() -> String {
        URL_SAFE_NO_PAD.encode([9u8; 32])
    }

    #[test]
    fn ip_forms_match_node_addr() {
        for addr in ["203.0.113.7:4433", "[2001:db8::1]:4433"] {
            let text = format!("{}@{addr}", key());
            let name: NodeName = text.parse().unwrap();
            assert_eq!(name.to_string(), text);
            assert_eq!(name.addr(), Some(addr.parse().unwrap()));
            assert_eq!(name.host_name(), None);
        }
    }

    #[test]
    fn names_parse_and_round_trip() {
        let text = format!("{}@Relay.Example.org:4433", key());
        let name: NodeName = text.parse().unwrap();
        assert_eq!(name.host_name(), Some("relay.example.org"));
        assert_eq!(name.port(), 4433);
        assert_eq!(name.addr(), None);
        assert_eq!(name.to_string().parse::<NodeName>().unwrap(), name);
        assert!(format!("{}@localhost:1", key()).parse::<NodeName>().is_ok());
    }

    #[test]
    fn bad_forms_are_refused() {
        let key = key();
        for text in [
            "relay.example.org:4433".to_string(),
            format!("{key}@relay.example.org"),
            format!("{key}@relay.example.org:99999"),
            format!("{key}@:4433"),
            format!("{key}@203.0.113.999:4433"),
            format!("{key}@1.2.3:4433"),
            format!("{key}@-relay.example.org:4433"),
            format!("{key}@relay..example.org:4433"),
            format!("{key}@relay.example.org.:4433"),
            format!("{key}@relay_1.example.org:4433"),
            format!("{key}@relay example.org:4433"),
            format!("{key}@{}.org:4433", "a".repeat(64)),
            format!("{key}@{}:4433", "a.".repeat(60)),
            "AAAA@relay.example.org:4433".to_string(),
        ] {
            assert!(text.parse::<NodeName>().is_err(), "{text}");
        }
    }

    #[tokio::test]
    async fn ip_resolves_without_lookup_and_unspecified_is_refused() {
        let name: NodeName = format!("{}@203.0.113.7:4433", key()).parse().unwrap();
        assert_eq!(
            name.resolve().await.unwrap().addr,
            "203.0.113.7:4433".parse().unwrap()
        );
        for addr in ["0.0.0.0:4433", "[::]:4433", "203.0.113.7:0"] {
            let name: NodeName = format!("{}@{addr}", key()).parse().unwrap();
            assert!(name.resolve().await.is_err(), "{addr}");
        }
    }

    #[tokio::test]
    async fn names_resolve_ipv4_first_and_keep_the_key() {
        let name: NodeName = format!("{}@localhost:4433", key()).parse().unwrap();
        let node = name.resolve().await.unwrap();
        assert_eq!(node.key, name.key);
        assert_eq!(node.addr, "127.0.0.1:4433".parse().unwrap());
        // `.invalid` never resolves (RFC 2606).
        let name: NodeName = format!("{}@veil.invalid:4433", key()).parse().unwrap();
        assert!(name.resolve().await.is_err());
    }

    #[tokio::test]
    async fn ipv6_lookup_gives_public_ipv6_only() {
        let named = |addr: &str| format!("{}@{addr}", key()).parse::<NodeName>().unwrap();
        let public = named("[2a02:c7f::9]:4433");
        assert_eq!(
            public.resolve_ipv6().await.unwrap().addr,
            "[2a02:c7f::9]:4433".parse().unwrap()
        );
        for addr in ["203.0.113.7:4433", "[::]:4433", "[fe80::1]:4433", "[2a02:c7f::9]:0"] {
            assert!(named(addr).resolve_ipv6().await.is_none(), "{addr}");
        }
    }

    #[test]
    fn usable_skips_unspecified_and_prefers_ipv4() {
        let addrs: Vec<SocketAddr> = ["0.0.0.0:1", "[2001:db8::1]:1", "198.51.100.1:1"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect();
        assert_eq!(usable(addrs), Some("198.51.100.1:1".parse().unwrap()));
        assert_eq!(usable(["[::]:1".parse().unwrap()]), None);
    }
}
