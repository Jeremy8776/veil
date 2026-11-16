//! Network settings, the addresses an invite gives out, and the host's plan for each epoch.
//!
//! Purpose: pure helpers for `host.rs` and `guest.rs`, tested without a network.
//! - [`NetConfig`]: optional settings from the environment, for calls across the internet.
//!   `VEIL_RENDEZVOUS=KEY@IP:PORT` uses a `veil-node` rendezvous (and relay) instead of the
//!   host's own; `VEIL_ADVERTISE=IP:PORT,...` adds addresses guests can dial directly (a public
//!   IP with a forwarded port); `VEIL_PORT` fixes the host's port (default: any free port);
//!   `VEIL_FORWARDER=KEY@IP:PORT` carries large calls through a `veil-node` forwarder.
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
use veil_net::NodeAddr;
use veil_security::PublicId;
use veil_topology::{NodeId, NodeKind, Plan, Policy, plan};

const ASSUMED_KBPS: u64 = 100_000;

/// Planner id for the forwarder; member ids are their leaf indexes, far below this.
const FORWARDER_ID: NodeId = NodeId(u32::MAX);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetConfig {
    pub rendezvous: Option<NodeAddr>,
    pub forwarder: Option<NodeAddr>,
    pub advertise: Vec<SocketAddr>,
    pub port: u16,
}

impl NetConfig {
    /// Reads the settings; a value that does not parse is ignored with a note on stderr.
    pub fn from_env() -> Self {
        let get = |name: &str| std::env::var(name).ok();
        let (mut config, mut problems) = Self::parse(
            get("VEIL_RENDEZVOUS").as_deref(),
            get("VEIL_ADVERTISE").as_deref(),
            get("VEIL_PORT").as_deref(),
        );
        config.forwarder = node_addr(
            "VEIL_FORWARDER",
            get("VEIL_FORWARDER").as_deref(),
            &mut problems,
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
        config.rendezvous = node_addr("VEIL_RENDEZVOUS", rendezvous, &mut problems);
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

/// A `KEY@IP:PORT` setting; blank is none, and a bad value is a problem and none.
fn node_addr(name: &str, text: Option<&str>, problems: &mut Vec<String>) -> Option<NodeAddr> {
    let text = text.map(str::trim).filter(|t| !t.is_empty())?;
    let parsed = text.parse().ok();
    if parsed.is_none() {
        problems.push(format!("{name} {text:?}: want KEY@IP:PORT"));
    }
    parsed
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

    #[test]
    fn a_bad_forwarder_setting_is_reported() {
        let mut problems = Vec::new();
        assert!(node_addr("VEIL_FORWARDER", Some("nonsense"), &mut problems).is_none());
        assert!(node_addr("VEIL_FORWARDER", Some("  "), &mut problems).is_none());
        assert_eq!(problems.len(), 1);
        let text = forwarder().to_string();
        assert!(node_addr("VEIL_FORWARDER", Some(&text), &mut problems).is_some());
    }
}
