//! The node's configuration file (TOML).
//!
//! Example:
//!
//! ```toml
//! # Created on first start. Keep it private: it is this node's identity.
//! identity_file = "node.key"
//!
//! [rendezvous]
//! listen = "0.0.0.0:4433"
//! relay = true            # default true
//!
//! [forwarder]
//! listen = "0.0.0.0:4434"
//! max_rooms = 1000        # optional
//! max_members_per_room = 16
//! ```
//!
//! Invariants: unknown keys are refused (a typo must not silently turn a role off), at least
//! one role is on, two roles never share a port, and the file is bounded before parsing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use veil_forward::Limits;

use crate::NodeError;

pub const MAX_CONFIG_LEN: usize = 64 * 1024;
/// Members a forwarder room may be configured for. Calls are scoped to about 16 people; the
/// headroom covers recorders and gateways, which join as members.
pub const MAX_ROOM_MEMBERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub identity_file: PathBuf,
    pub rendezvous: Option<RendezvousRole>,
    pub forwarder: Option<ForwarderRole>,
}

/// Address reflection, introductions for hole punching and, unless turned off, relaying.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendezvousRole {
    pub listen: SocketAddr,
    #[serde(default = "yes")]
    pub relay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForwarderRole {
    pub listen: SocketAddr,
    pub max_rooms: Option<usize>,
    pub max_members_per_room: Option<usize>,
}

fn yes() -> bool {
    true
}

impl ForwarderRole {
    pub fn limits(&self) -> Limits {
        let defaults = Limits::default();
        Limits {
            max_rooms: self.max_rooms.unwrap_or(defaults.max_rooms),
            max_members_per_room: self
                .max_members_per_room
                .unwrap_or(defaults.max_members_per_room),
            queue_bytes: defaults.queue_bytes,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, NodeError> {
        if text.len() > MAX_CONFIG_LEN {
            return Err(NodeError::Config("file is larger than 64 KiB".into()));
        }
        let config: Self = toml::from_str(text).map_err(|e| NodeError::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Reads and parses a file. A relative `identity_file` is resolved next to the config file.
    pub fn load(path: &Path) -> Result<Self, NodeError> {
        let size = std::fs::metadata(path).map_err(|e| NodeError::Io(path.into(), e))?.len();
        if size > MAX_CONFIG_LEN as u64 {
            return Err(NodeError::Config("file is larger than 64 KiB".into()));
        }
        let text = std::fs::read_to_string(path).map_err(|e| NodeError::Io(path.into(), e))?;
        let mut config = Self::parse(&text)?;
        if config.identity_file.is_relative() {
            if let Some(dir) = path.parent() {
                config.identity_file = dir.join(&config.identity_file);
            }
        }
        Ok(config)
    }

    fn validate(&self) -> Result<(), NodeError> {
        let bad = |why: &str| Err(NodeError::Config(why.into()));
        if self.rendezvous.is_none() && self.forwarder.is_none() {
            return bad("turn on at least one role: [rendezvous] or [forwarder]");
        }
        if let (Some(r), Some(f)) = (&self.rendezvous, &self.forwarder) {
            if r.listen.port() != 0 && r.listen.port() == f.listen.port() {
                return bad("each role needs its own port");
            }
        }
        if let Some(f) = &self.forwarder {
            let limits = f.limits();
            if limits.max_rooms == 0 {
                return bad("forwarder.max_rooms must be at least 1");
            }
            if !(2..=MAX_ROOM_MEMBERS).contains(&limits.max_members_per_room) {
                return bad("forwarder.max_members_per_room must be between 2 and 64");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const FULL: &str = r#"
identity_file = "node.key"

[rendezvous]
listen = "0.0.0.0:4433"

[forwarder]
listen = "0.0.0.0:4434"
max_members_per_room = 8
"#;

    #[test]
    fn full_example_parses_with_defaults() {
        let config = Config::parse(FULL).unwrap();
        let rendezvous = config.rendezvous.unwrap();
        assert!(rendezvous.relay, "relay defaults to on");
        let limits = config.forwarder.unwrap().limits();
        assert_eq!(limits.max_members_per_room, 8);
        assert_eq!(limits.max_rooms, Limits::default().max_rooms);
    }

    #[test]
    fn relay_can_be_turned_off() {
        let text = "identity_file = \"k\"\n[rendezvous]\nlisten = \"0.0.0.0:1\"\nrelay = false\n";
        assert!(!Config::parse(text).unwrap().rendezvous.unwrap().relay);
    }

    #[test]
    fn mistakes_are_refused() {
        for text in [
            // No role.
            "identity_file = \"k\"\n",
            // Typo in a key.
            "identity_file = \"k\"\n[rendezvous]\nlisten = \"0.0.0.0:1\"\nrelais = false\n",
            // Unknown section.
            "identity_file = \"k\"\n[recorder]\nlisten = \"0.0.0.0:1\"\n",
            // Shared port.
            "identity_file = \"k\"\n[rendezvous]\nlisten = \"0.0.0.0:5\"\n[forwarder]\nlisten = \"0.0.0.0:5\"\n",
            // Bad limits.
            "identity_file = \"k\"\n[forwarder]\nlisten = \"0.0.0.0:5\"\nmax_members_per_room = 1\n",
            "identity_file = \"k\"\n[forwarder]\nlisten = \"0.0.0.0:5\"\nmax_rooms = 0\n",
            // Not an address.
            "identity_file = \"k\"\n[forwarder]\nlisten = \"localhost\"\n",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
        assert!(Config::parse(&"#".repeat(MAX_CONFIG_LEN + 1)).is_err());
    }
}
