//! The node's configuration file (TOML).
//!
//! Example:
//!
//! ```toml
//! # Created on first start. Keep it private: it is this node's identity.
//! identity_file = "node.key"
//! # Optional: only these device keys may host calls here. Their guests still join.
//! # Leave it out to let anyone host.
//! hosts = ["BASE64URL_DEVICE_KEY"]
//!
//! [rendezvous]
//! listen = "0.0.0.0:4433"
//! relay = true            # default true
//!
//! [forwarder]
//! listen = "0.0.0.0:4434"
//! max_rooms = 4096        # optional
//! max_rooms_per_block = 16 # optional: rooms one remote address may hold open
//! max_members_per_room = 16
//! node_queue_mib = 1024   # optional: video all receivers together may have queued
//! source_queue_mib = 128  # optional: share of that one remote address may hold
//! ```
//!
//! Invariants: unknown keys are refused (a typo must not silently turn a role off), at least
//! one role is on, two roles never share a port, and the file is bounded before parsing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use veil_forward::Limits;
use veil_security::PublicId;

use crate::NodeError;

pub const MAX_CONFIG_LEN: usize = 64 * 1024;
/// Members a forwarder room may be configured for. Calls are scoped to about 16 people; the
/// headroom covers recorders and gateways, which join as members.
pub const MAX_ROOM_MEMBERS: usize = 64;
/// Most keys `hosts` may list. Far more than a person's friends; bounds the work at startup.
pub const MAX_HOSTS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub identity_file: PathBuf,
    /// Device keys allowed to host, as text; `None` lets anyone host. Read with
    /// [`Config::host_keys`], which `validate` has already checked.
    pub hosts: Option<Vec<String>>,
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
    /// Rooms that links from one remote address (IPv4 address or IPv6 /64) may hold open.
    pub max_rooms_per_block: Option<usize>,
    pub max_members_per_room: Option<usize>,
    /// MiB of video the forwarder may hold queued for all receivers together.
    pub node_queue_mib: Option<usize>,
    /// MiB of that budget one remote address (IPv4 address or IPv6 /64) may hold.
    pub source_queue_mib: Option<usize>,
}

const MIB: usize = 1024 * 1024;

fn is_zero(v: Option<usize>) -> bool {
    v == Some(0)
}

fn yes() -> bool {
    true
}

impl ForwarderRole {
    pub fn limits(&self) -> Limits {
        let defaults = Limits::default();
        Limits {
            max_rooms: self.max_rooms.unwrap_or(defaults.max_rooms),
            max_rooms_per_block: self
                .max_rooms_per_block
                .unwrap_or(defaults.max_rooms_per_block),
            max_members_per_room: self
                .max_members_per_room
                .unwrap_or(defaults.max_members_per_room),
            node_queue_bytes: self
                .node_queue_mib
                .map_or(defaults.node_queue_bytes, |m| m.saturating_mul(MIB)),
            source_queue_bytes: self
                .source_queue_mib
                .map_or(defaults.source_queue_bytes, |m| m.saturating_mul(MIB)),
            ..defaults
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
        if config.identity_file.is_relative()
            && let Some(dir) = path.parent()
        {
            config.identity_file = dir.join(&config.identity_file);
        }
        Ok(config)
    }

    /// The keys allowed to host, or `None` when anyone may.
    pub fn host_keys(&self) -> Result<Option<Vec<PublicId>>, NodeError> {
        let Some(hosts) = &self.hosts else {
            return Ok(None);
        };
        hosts
            .iter()
            .map(|text| {
                veil_net::node_addr::parse_key(text).map_err(|_| {
                    NodeError::Config(format!(
                        "hosts: {text:?} is not a device key (43 base64url characters)"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    fn validate(&self) -> Result<(), NodeError> {
        let bad = |why: &str| Err(NodeError::Config(why.into()));
        if let Some(hosts) = &self.hosts {
            // An empty list would let nobody host: almost certainly a mistake.
            if hosts.is_empty() {
                return bad("hosts is empty, so nobody could host: list keys or remove the line");
            }
            if hosts.len() > MAX_HOSTS {
                return bad("hosts lists more than 1024 keys");
            }
            self.host_keys()?;
        }
        if self.rendezvous.is_none() && self.forwarder.is_none() {
            return bad("turn on at least one role: [rendezvous] or [forwarder]");
        }
        if let (Some(r), Some(f)) = (&self.rendezvous, &self.forwarder)
            && r.listen.port() != 0
            && r.listen.port() == f.listen.port()
        {
            return bad("each role needs its own port");
        }
        if let Some(f) = &self.forwarder {
            let limits = f.limits();
            if limits.max_rooms == 0 {
                return bad("forwarder.max_rooms must be at least 1");
            }
            if limits.max_rooms_per_block == 0 {
                return bad("forwarder.max_rooms_per_block must be at least 1");
            }
            if !(2..=MAX_ROOM_MEMBERS).contains(&limits.max_members_per_room) {
                return bad("forwarder.max_members_per_room must be between 2 and 64");
            }
            if is_zero(f.node_queue_mib) {
                return bad("forwarder.node_queue_mib must be at least 1");
            }
            if is_zero(f.source_queue_mib) {
                return bad("forwarder.source_queue_mib must be at least 1");
            }
            if limits.source_queue_bytes > limits.node_queue_bytes {
                return bad("forwarder.source_queue_mib must not exceed node_queue_mib");
            }
            if limits.node_queue_bytes < limits.queue_bytes {
                return bad("forwarder.node_queue_mib must be at least the per-receiver queue (32 MiB)");
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
        assert_eq!(limits.max_rooms_per_block, Limits::default().max_rooms_per_block);
        let set = forwarder("max_rooms_per_block = 3\n");
        assert_eq!(Config::parse(&set).unwrap().forwarder.unwrap().limits().max_rooms_per_block, 3);
    }

    fn forwarder(extra: &str) -> String {
        format!("identity_file = \"k\"
[forwarder]
listen = \"0.0.0.0:5\"
{extra}")
    }

    #[test]
    fn queue_budgets_default_to_forwarder_defaults() {
        let limits = Config::parse(&forwarder("")).unwrap().forwarder.unwrap().limits();
        let d = Limits::default();
        assert_eq!(limits.node_queue_bytes, d.node_queue_bytes);
        assert_eq!(limits.source_queue_bytes, d.source_queue_bytes);
        assert_eq!(limits.queue_bytes, d.queue_bytes);
        assert_eq!(limits.send_deadline, d.send_deadline);
    }

    #[test]
    fn queue_budgets_parse_in_mib() {
        let text = forwarder("node_queue_mib = 512
source_queue_mib = 64
");
        let limits = Config::parse(&text).unwrap().forwarder.unwrap().limits();
        assert_eq!(limits.node_queue_bytes, 512 * 1024 * 1024);
        assert_eq!(limits.source_queue_bytes, 64 * 1024 * 1024);
        let equal = forwarder("node_queue_mib = 64
source_queue_mib = 64
");
        assert!(Config::parse(&equal).is_ok());
    }

    #[test]
    fn bad_queue_budgets_are_refused() {
        for extra in [
            "node_queue_mib = 0
",
            "source_queue_mib = 0
",
            // Source share above the node budget.
            "node_queue_mib = 64
source_queue_mib = 65
",
            // Default source share (128) above a small node budget.
            "node_queue_mib = 64
",
            // Node budget below one receiver's queue.
            "node_queue_mib = 16
source_queue_mib = 8
",
            "node_queue_mib = -1
",
            "node_queue_mib = \"big\"
",
        ] {
            assert!(Config::parse(&forwarder(extra)).is_err(), "{extra}");
        }
    }

    #[test]
    fn huge_queue_budget_does_not_overflow() {
        let text = forwarder("node_queue_mib = 18446744073709551615
source_queue_mib = 1
");
        let limits = Config::parse(&text).unwrap().forwarder.unwrap().limits();
        assert_eq!(limits.node_queue_bytes, usize::MAX);
    }

    #[test]
    fn hosts_are_optional_and_checked() {
        let key = veil_net::node_addr::key_text(&PublicId([7; 32]));
        let with = |hosts: &str| format!("identity_file = \"k\"\nhosts = {hosts}\n[rendezvous]\nlisten = \"0.0.0.0:1\"\n");
        assert_eq!(Config::parse(FULL).unwrap().host_keys().unwrap(), None, "anyone by default");
        let one = Config::parse(&with(&format!("[\"{key}\"]"))).unwrap();
        assert_eq!(one.host_keys().unwrap(), Some(vec![PublicId([7; 32])]));
        for bad in ["[]", "[\"not-a-key\"]", "\"one\"", &format!("[\"{key}x\"]")] {
            assert!(Config::parse(&with(bad)).is_err(), "{bad}");
        }
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
            "identity_file = \"k\"\n[forwarder]\nlisten = \"0.0.0.0:5\"\nmax_rooms_per_block = 0\n",
            // Not an address.
            "identity_file = \"k\"\n[forwarder]\nlisten = \"localhost\"\n",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
        assert!(Config::parse(&"#".repeat(MAX_CONFIG_LEN + 1)).is_err());
    }
}
