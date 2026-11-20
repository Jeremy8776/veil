//! A node's address with its key: `<base64url key>@<ip>:<port>`.
//!
//! Every dial must name the key it expects, so an address alone is never enough to reach a
//! relay, forwarder or rendezvous node. This is the text form an operator copies from
//! `veil-node` into an invite's relay field.
//!
//! The address part is a literal IP and port, the form invites and plans carry. A node named
//! by DNS in settings is [`crate::NodeName`], resolved to this form when a call starts.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use veil_security::PublicId;

use crate::NetError;

/// Longest accepted text form: 43 key characters, `@`, and an IPv6 address or host name with
/// port. [`crate::NodeName`] uses the same bound.
pub const MAX_NODE_ADDR_LEN: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeAddr {
    pub key: PublicId,
    pub addr: SocketAddr,
}

impl fmt::Display for NodeAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", key_text(&self.key), self.addr)
    }
}

impl FromStr for NodeAddr {
    type Err = NetError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() > MAX_NODE_ADDR_LEN {
            return Err(NetError::TooLarge);
        }
        let (key, addr) = text
            .trim()
            .split_once('@')
            .ok_or(NetError::Protocol("node address needs key@address"))?;
        let key = parse_key(key)?;
        let addr = addr
            .parse()
            .map_err(|_| NetError::Protocol("node address is not ip:port"))?;
        Ok(Self { key, addr })
    }
}

/// A device or node key in the same text form as the part before `@`: 43 base64url
/// characters. Used where a key stands alone, such as a node's list of allowed hosts.
pub fn key_text(key: &PublicId) -> String {
    URL_SAFE_NO_PAD.encode(key.0)
}

/// Reads a key written by [`key_text`].
pub fn parse_key(text: &str) -> Result<PublicId, NetError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text.trim())
        .map_err(|_| NetError::Protocol("node key is not base64url"))?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| NetError::Protocol("node key is not 32 bytes"))?;
    Ok(PublicId(key))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_v4_and_v6() {
        for addr in ["203.0.113.7:4433", "[2001:db8::1]:4433"] {
            let node = NodeAddr {
                key: PublicId([9; 32]),
                addr: addr.parse().unwrap(),
            };
            assert_eq!(node.to_string().parse::<NodeAddr>().unwrap(), node);
        }
    }

    #[test]
    fn bad_forms_are_refused() {
        let key = URL_SAFE_NO_PAD.encode([9u8; 32]);
        for text in [
            "203.0.113.7:4433".to_string(),
            format!("{key}@relay.example.org:4433"),
            format!("{key}@203.0.113.7"),
            "AAAA@203.0.113.7:4433".to_string(),
            "!!@203.0.113.7:4433".to_string(),
            format!("{key}@{}", "1".repeat(200)),
        ] {
            assert!(text.parse::<NodeAddr>().is_err(), "{text}");
        }
    }
}
