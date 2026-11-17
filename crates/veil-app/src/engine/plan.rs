//! Network settings, the addresses an invite gives out, and the host's plan for each epoch.
//!
//! Purpose: pure helpers for `host.rs` and `guest.rs`, tested without a network.
//! - [`NetConfig`]: optional settings for calls across the internet, saved from Settings >
//!   Connection (`crate::connection`), each overridden by its environment variable if set.
//!   `VEIL_RENDEZVOUS=KEY@HOST:PORT` uses a `veil-node` rendezvous (and relay) instead of the
//!   host's own; `VEIL_ADVERTISE=IP:PORT,...` adds addresses guests can dial directly (a public
//!   IP with a forwarded port); `VEIL_PORT` fixes the host's port (default: any free port);
//!   `VEIL_FORWARDER=KEY@HOST:PORT` carries large calls through a `veil-node` forwarder.
//! - [`parse_node`], [`parse_advertise`], [`parse_port`]: one field each, shared by both
//!   sources, refusing addresses nobody can dial (`0.0.0.0`, port 0). A node's host may be a
//!   DNS name; `host.rs` and `guest.rs` resolve it when the call starts.
//! - [`advertised`]: the invite's direct addresses. Each is given only if it can work.
//! - [`plan_for`]: this epoch's members as a mesh, or, past `Policy::mesh_max` (4 people) and
//!   with a forwarder known, every member through the forwarder.
//!
//! Invariants:
//! - Finding this machine's LAN address sends nothing: connecting a UDP socket only asks the
//!   routing table which interface would be used.
//! - The plan's directory lists exactly the roster, as `HostHandle::publish_plan` requires,
//!   plus the forwarder when there is one.
//!
//! shortcut: capacities are assumed, not measured, so the plan follows the policy alone: a
//! mesh up to 4 people, the forwarder above that. Measure links to plan by real capacity.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use veil_call::{Directory, Peer};
use veil_net::{NodeAddr, NodeName};
use veil_security::PublicId;
use veil_topology::{NodeId, NodeKind, Plan, Policy, plan};

const ASSUMED_KBPS: u64 = 100_000;

/// Planner id for the forwarder; member ids are their leaf indexes, far below this.
const FORWARDER_ID: NodeId = NodeId(u32::MAX);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetConfig {
    pub rendezvous: Option<NodeName>,
    pub forwarder: Option<NodeName>,
    pub advertise: Vec<SocketAddr>,
    pub port: u16,
}

/// The environment variable for each setting, in the order the settings screen shows them.
pub const ENV_RENDEZVOUS: &str = "VEIL_RENDEZVOUS";
pub const ENV_FORWARDER: &str = "VEIL_FORWARDER";
pub const ENV_ADVERTISE: &str = "VEIL_ADVERTISE";
pub const ENV_PORT: &str = "VEIL_PORT";

impl NetConfig {
    /// Starts from `self` (the saved settings) and lets each environment variable that is set
    /// replace its setting. A value that does not parse is ignored with a note in the log.
    pub fn with_env(self, get: impl Fn(&str) -> Option<String>) -> Self {
        let mut config = self;
        let set = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        let mut problems = Vec::new();
        let mut take = |name: &str, result: Result<(), String>| {
            if let Err(e) = result {
                problems.push(format!("{name}: {e}"));
            }
        };
        if let Some(text) = set(ENV_RENDEZVOUS) {
            take(
                ENV_RENDEZVOUS,
                parse_node(&text).map(|n| config.rendezvous = n),
            );
        }
        if let Some(text) = set(ENV_FORWARDER) {
            take(
                ENV_FORWARDER,
                parse_node(&text).map(|n| config.forwarder = n),
            );
        }
        if let Some(text) = set(ENV_ADVERTISE) {
            take(
                ENV_ADVERTISE,
                parse_advertise(&text).map(|a| config.advertise = a),
            );
        }
        if let Some(text) = set(ENV_PORT) {
            take(ENV_PORT, parse_port(&text).map(|p| config.port = p));
        }
        for problem in problems {
            crate::note!("ignoring {problem}");
        }
        config
    }
}

/// A node as `KEY@HOST:PORT`, the host an IP or a DNS name; blank is none. An IP must be one
/// a client can dial. A name is checked again when it is resolved (`NodeName::resolve`).
pub fn parse_node(text: &str) -> Result<Option<NodeName>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let node: NodeName = text.parse().map_err(|_| {
        String::from("Use KEY@IP:PORT as veil-node prints it, or KEY@NAME:PORT.")
    })?;
    match node.addr() {
        Some(addr) => dialable(addr)?,
        None if node.port() == 0 => return Err(String::from("Port 0 cannot be dialled.")),
        None => {}
    }
    Ok(Some(node))
}

/// Resolves a configured node when a call starts. `role` names it in the error the user sees.
pub async fn resolve(node: Option<&NodeName>, role: &str) -> Result<Option<NodeAddr>, String> {
    let Some(node) = node else {
        return Ok(None);
    };
    node.resolve()
        .await
        .map(Some)
        .map_err(|e| format!("Cannot reach the {role} server in Settings > Connection: {e}"))
}

/// Addresses guests may dial, as `IP:PORT` separated by commas; blank is none.
pub fn parse_advertise(text: &str) -> Result<Vec<SocketAddr>, String> {
    let mut addrs = Vec::new();
    for part in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let addr: SocketAddr = part
            .parse()
            .map_err(|_| format!("{part:?} is not IP:PORT."))?;
        dialable(addr)?;
        if !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }
    if addrs.len() > veil_security::invite::MAX_ADDRS {
        return Err(format!(
            "An invite holds at most {} addresses.",
            veil_security::invite::MAX_ADDRS
        ));
    }
    Ok(addrs)
}

/// The host's UDP port; blank is any free port.
pub fn parse_port(text: &str) -> Result<u16, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    match text.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(String::from(
            "Use a port from 1 to 65535, or leave it blank.",
        )),
    }
}

/// Refuses addresses nobody can dial. `veil-node` prints its listen address (`0.0.0.0`),
/// so pasting it unchanged is an easy mistake.
fn dialable(addr: SocketAddr) -> Result<(), String> {
    if addr.ip().is_unspecified() {
        return Err(format!(
            "{} is where a server listens, not an address to dial. Use its public IP.",
            addr.ip()
        ));
    }
    if addr.port() == 0 {
        return Err(String::from("Port 0 cannot be dialled."));
    }
    Ok(())
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

/// This epoch's plan and the directory behind it. Only the host's addresses are known, so in
/// a mesh members reach each other by punching through the rendezvous. With a `forwarder`, a
/// call larger than `Policy::mesh_max` goes through it instead.
pub fn plan_for(
    roster: &[(u32, PublicId)],
    me: PublicId,
    addrs: &[SocketAddr],
    forwarder: Option<NodeAddr>,
) -> Result<(Plan, Directory), String> {
    let mine = roster
        .iter()
        .find(|(_, d)| *d == me)
        .ok_or("the host is not in its own roster")?;
    let mut nodes: Vec<veil_topology::Node> = roster
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
    let mut policy = Policy::default();
    if forwarder.is_none() {
        // With nothing to forward through, the policy's mesh limit would make a large call
        // infeasible. A strained mesh is better than no plan.
        policy.mesh_max = policy.mesh_max.max(roster.len());
    } else {
        nodes.push(veil_topology::Node {
            id: FORWARDER_ID,
            kind: NodeKind::SelfHosted,
            uplink_kbps: ASSUMED_KBPS * 10,
            downlink_kbps: ASSUMED_KBPS * 10,
            participant: false,
            can_forward: true,
        });
    }
    let plan = plan(&nodes, &[], &policy).map_err(|e| format!("cannot plan: {e:?}"))?;
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
        forwarders: forwarder.map(|f| (FORWARDER_ID, f)).into_iter().collect(),
    };
    Ok((plan, directory))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use veil_security::Identity;

    use super::*;

    fn node_text(addr: &str) -> String {
        NodeAddr {
            key: Identity::generate().public(),
            addr: addr.parse().unwrap(),
        }
        .to_string()
    }

    #[test]
    fn fields_parse_and_blank_means_none() {
        let text = node_text("203.0.113.5:4433");
        assert_eq!(parse_node(&text).unwrap().unwrap().to_string(), text);
        assert_eq!(parse_node("  ").unwrap(), None);
        let addrs =
            parse_advertise("198.51.100.2:7710, [2001:db8::1]:7710,198.51.100.2:7710").unwrap();
        assert_eq!(addrs.len(), 2, "duplicates are dropped");
        assert!(parse_advertise("").unwrap().is_empty());
        assert_eq!(parse_port("7710").unwrap(), 7710);
        assert_eq!(parse_port(" ").unwrap(), 0);
    }

    #[test]
    fn nodes_may_be_named_by_dns() {
        let ip = node_text("203.0.113.5:4433");
        let key = ip.split_once('@').unwrap().0;
        let text = format!("{key}@veil.example.org:4433");
        let node = parse_node(&text).unwrap().unwrap();
        assert_eq!(node.host_name(), Some("veil.example.org"));
        assert_eq!(node.to_string(), text);
        assert!(parse_node(&format!("{key}@veil.example.org:0")).is_err());
        assert!(parse_node(&format!("{key}@veil.example.org")).is_err());
        assert!(parse_node(&format!("{key}@veil_example.org:4433")).is_err());
    }

    #[test]
    fn addresses_nobody_can_dial_are_refused() {
        assert!(parse_node("nonsense").is_err());
        assert!(
            parse_node(&node_text("0.0.0.0:4433")).is_err(),
            "veil-node's listen address"
        );
        assert!(parse_node(&node_text("[::]:4433")).is_err());
        assert!(parse_node(&node_text("203.0.113.5:0")).is_err());
        assert!(parse_advertise("198.51.100.2:7710, nonsense").is_err());
        assert!(parse_advertise("0.0.0.0:7710").is_err());
        let many: Vec<String> = (1..=veil_security::invite::MAX_ADDRS + 1)
            .map(|i| format!("198.51.100.{i}:7710"))
            .collect();
        assert!(parse_advertise(&many.join(",")).is_err());
        for bad in ["0", "65536", "-1", "port"] {
            assert!(parse_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_environment_overrides_each_saved_setting_it_sets() {
        let saved = NetConfig {
            rendezvous: parse_node(&node_text("203.0.113.5:4433")).unwrap(),
            advertise: vec!["198.51.100.2:7710".parse().unwrap()],
            port: 7710,
            ..NetConfig::default()
        };
        let forwarder = node_text("203.0.113.6:4434");
        let env = |name: &str| match name {
            ENV_FORWARDER => Some(forwarder.clone()),
            ENV_PORT => Some(String::from("9000")),
            ENV_ADVERTISE => Some(String::from("   ")),
            ENV_RENDEZVOUS => Some(String::from("nonsense")),
            _ => None,
        };
        let config = saved.clone().with_env(env);
        assert_eq!(config.port, 9000);
        assert_eq!(
            config.forwarder.map(|f| f.to_string()),
            Some(forwarder.clone())
        );
        assert_eq!(
            config.advertise, saved.advertise,
            "a blank variable is not set"
        );
        assert_eq!(
            config.rendezvous, saved.rendezvous,
            "a bad variable is ignored"
        );
        assert_eq!(
            NetConfig::default().with_env(|_| None),
            NetConfig::default()
        );
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
        let (plan, directory) = plan_for(&[(0, host), (1, guest)], host, &addrs, None).unwrap();
        assert!(matches!(plan, Plan::Mesh { .. }), "{plan:?}");
        assert_eq!(directory.members[&NodeId(0)].addrs.len(), 1);
        assert!(directory.members[&NodeId(1)].addrs.is_empty());
        assert!(plan_for(&[(1, guest)], host, &addrs, None).is_err());
    }

    fn roster(n: u32) -> Vec<(u32, PublicId)> {
        (0..n)
            .map(|leaf| (leaf, Identity::generate().public()))
            .collect()
    }

    fn forwarder() -> NodeAddr {
        NodeAddr {
            key: Identity::generate().public(),
            addr: "192.168.1.20:5001".parse().unwrap(),
        }
    }

    #[test]
    fn four_people_stay_a_mesh_even_with_a_forwarder() {
        let roster = roster(4);
        let (plan, _) = plan_for(&roster, roster[0].1, &[], Some(forwarder())).unwrap();
        assert!(matches!(plan, Plan::Mesh { .. }), "{plan:?}");
    }

    #[test]
    fn five_people_go_through_the_forwarder() {
        let roster = roster(5);
        let fwd = forwarder();
        let (plan, directory) = plan_for(&roster, roster[2].1, &[], Some(fwd)).unwrap();
        let Plan::Forwarded { forwarders, .. } = &plan else {
            panic!("{plan:?}");
        };
        assert_eq!(forwarders, &[FORWARDER_ID]);
        assert_eq!(directory.forwarders[&FORWARDER_ID], fwd);
        assert_eq!(directory.members.len(), 5);
        assert_eq!(directory.me, NodeId(2));
    }

    #[test]
    fn without_a_forwarder_a_large_call_is_still_a_mesh() {
        let roster = roster(6);
        let (plan, directory) = plan_for(&roster, roster[0].1, &[], None).unwrap();
        assert!(matches!(plan, Plan::Mesh { .. }), "{plan:?}");
        assert!(directory.forwarders.is_empty());
    }
}
