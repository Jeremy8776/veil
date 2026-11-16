//! Network settings, the addresses an invite gives out, and the host's plan for each epoch.
//!
//! Purpose: pure helpers for `host.rs` and `guest.rs`, tested without a network.
//! - [`NetConfig`]: optional settings from the environment, for calls across the internet.
//!   `VEIL_RENDEZVOUS=KEY@IP:PORT` uses a `veil-node` rendezvous (and relay) instead of the
//!   host's own; `VEIL_ADVERTISE=IP:PORT,...` adds addresses guests can dial directly (a public
//!   IP with a forwarded port); `VEIL_PORT` fixes the host's port (default: any free port).
//! - [`advertised`]: the invite's direct addresses. Each is given only if it can work.
//! - [`plan_for`]: a mesh of this epoch's members, as `veil-probe` plans it.
//!
//! Invariants:
//! - Finding this machine's LAN address sends nothing: connecting a UDP socket only asks the
//!   routing table which interface would be used.
//! - The plan's directory lists exactly the roster, as `HostHandle::publish_plan` requires.
//!
//! shortcut: capacities are assumed, not measured, so the plan follows the policy alone and is
//! always a mesh. Measure links and allow a forwarder when calls grow past a handful.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use veil_call::{Directory, Peer};
use veil_net::NodeAddr;
use veil_security::PublicId;
use veil_topology::{NodeId, NodeKind, Plan, Policy, plan};

const ASSUMED_KBPS: u64 = 100_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetConfig {
    pub rendezvous: Option<NodeAddr>,
    pub advertise: Vec<SocketAddr>,
    pub port: u16,
}

impl NetConfig {
    /// Reads the settings; a value that does not parse is ignored with a note on stderr.
    pub fn from_env() -> Self {
        let get = |name: &str| std::env::var(name).ok();
        let (config, problems) = Self::parse(
            get("VEIL_RENDEZVOUS").as_deref(),
            get("VEIL_ADVERTISE").as_deref(),
            get("VEIL_PORT").as_deref(),
        );
        for problem in problems {
            eprintln!("veil-app: ignoring {problem}");
        }
        config
    }

    pub fn parse(
        rendezvous: Option<&str>,
        advertise: Option<&str>,
        port: Option<&str>,
    ) -> (Self, Vec<String>) {
        let mut config = Self::default();
        let mut problems = Vec::new();
        if let Some(text) = rendezvous.map(str::trim).filter(|t| !t.is_empty()) {
            match text.parse() {
                Ok(addr) => config.rendezvous = Some(addr),
                Err(_) => problems.push(format!("VEIL_RENDEZVOUS {text:?}: want KEY@IP:PORT")),
            }
        }
        for text in advertise
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            match text.parse() {
                Ok(addr) => config.advertise.push(addr),
                Err(_) => problems.push(format!("VEIL_ADVERTISE {text:?}: want IP:PORT")),
            }
        }
        if let Some(text) = port.map(str::trim).filter(|t| !t.is_empty()) {
            match text.parse() {
                Ok(port) => config.port = port,
                Err(_) => problems.push(format!("VEIL_PORT {text:?}: want a port number")),
            }
        }
        (config, problems)
    }
}

/// This machine's address on its local network, if it has one.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A documentation address (TEST-NET-1): nothing is sent, the route is only looked up.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// Addresses for the invite: configured ones first, then the LAN address, then this machine
/// itself (for a second copy of Veil on the same computer).
pub fn advertised(config: &NetConfig, lan: Option<IpAddr>, port: u16) -> Vec<SocketAddr> {
    let mut addrs = config.advertise.clone();
    if let Some(ip) = lan {
        addrs.push(SocketAddr::new(ip, port));
    }
    addrs.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port));
    addrs.dedup();
    addrs.truncate(veil_security::invite::MAX_ADDRS);
    addrs
}

/// This epoch's mesh and the directory behind it. Only the host's addresses are known, so
/// members reach each other by punching through the rendezvous.
pub fn plan_for(
    roster: &[(u32, PublicId)],
    me: PublicId,
    addrs: &[SocketAddr],
) -> Result<(Plan, Directory), String> {
    let mine = roster
        .iter()
        .find(|(_, d)| *d == me)
        .ok_or("the host is not in its own roster")?;
    let nodes: Vec<veil_topology::Node> = roster
        .iter()
        .map(|(leaf, _)| veil_topology::Node {
            id: NodeId(*leaf),
            kind: NodeKind::Peer,
            uplink_kbps: ASSUMED_KBPS,
            downlink_kbps: ASSUMED_KBPS,
            participant: true,
            can_forward: false,
        })
        .collect();
    let plan = plan(&nodes, &[], &Policy::default()).map_err(|e| format!("cannot plan: {e:?}"))?;
    let directory = Directory {
        me: NodeId(mine.0),
        members: roster
            .iter()
            .map(|(leaf, device)| {
                let addrs = if *device == me {
                    addrs.to_vec()
                } else {
                    Vec::new()
                };
                (
                    NodeId(*leaf),
                    Peer {
                        device: *device,
                        addrs,
                    },
                )
            })
            .collect(),
        forwarders: Default::default(),
    };
    Ok((plan, directory))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_security::Identity;

    use super::*;

    #[test]
    fn settings_parse_and_bad_values_are_reported() {
        let key = Identity::generate().public();
        let rendezvous = NodeAddr {
            key,
            addr: "203.0.113.5:4433".parse().unwrap(),
        }
        .to_string();
        let (config, problems) = NetConfig::parse(
            Some(&rendezvous),
            Some("198.51.100.2:7710, nonsense"),
            Some("7710"),
        );
        assert_eq!(config.rendezvous.map(|r| r.key), Some(key));
        assert_eq!(config.advertise.len(), 1);
        assert_eq!(config.port, 7710);
        assert_eq!(problems.len(), 1, "{problems:?}");
        let (empty, none) = NetConfig::parse(None, Some(""), None);
        assert_eq!(empty, NetConfig::default());
        assert!(none.is_empty());
    }

    #[test]
    fn invites_offer_configured_lan_then_local_addresses() {
        let config = NetConfig {
            advertise: vec!["198.51.100.2:7710".parse().unwrap()],
            ..NetConfig::default()
        };
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let addrs = advertised(&config, Some(lan), 5000);
        let text: Vec<String> = addrs.iter().map(ToString::to_string).collect();
        assert_eq!(
            text,
            ["198.51.100.2:7710", "192.168.1.20:5000", "127.0.0.1:5000"]
        );
        assert_eq!(advertised(&NetConfig::default(), None, 1).len(), 1);
    }

    #[test]
    fn two_people_make_a_mesh_with_the_hosts_addresses() {
        let (host, guest) = (Identity::generate().public(), Identity::generate().public());
        let addrs = ["127.0.0.1:5000".parse().unwrap()];
        let (plan, directory) = plan_for(&[(0, host), (1, guest)], host, &addrs).unwrap();
        assert!(matches!(plan, Plan::Mesh { .. }), "{plan:?}");
        assert_eq!(directory.members[&NodeId(0)].addrs.len(), 1);
        assert!(directory.members[&NodeId(1)].addrs.is_empty());
        assert!(plan_for(&[(1, guest)], host, &addrs).is_err());
    }
}
