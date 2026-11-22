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
//!   A public IPv6 address comes straight after the configured ones: guests dial IPv6
//!   first (`veil_net::family::dial_order`), and older versions skip what they cannot dial.
//! - [`plan_for_with_addresses`]: this epoch's members as a mesh, or, past
//!   `Policy::mesh_max` (4 people) and with a forwarder known, a proposed forwarded plan.
//!   `host.rs` checks every member's access before publishing that proposal.
//!
//! Invariants:
//! - Finding this machine's addresses enumerates interfaces without sending packets or
//!   needing a default internet route.
//! - The plan's directory lists exactly the roster, as `HostHandle::publish_plan` requires,
//!   plus the forwarder when there is one.
//!
//! shortcut: capacities are assumed, not measured, so the plan follows the policy alone: a
//! mesh up to 4 people, the forwarder above that. Measure links to plan by real capacity.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use veil_call::{Directory, Peer};
use veil_net::{NodeAddr, NodeName};
use veil_security::PublicId;
use veil_topology::{NodeId, NodeKind, Plan, Policy, plan};

const ASSUMED_KBPS: u64 = 100_000;

/// Planner id for the forwarder; member ids are their leaf indexes, far below this.
const FORWARDER_ID: NodeId = NodeId(u32::MAX);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetConfig {
    /// Restrict all call dials to connected local networks; ignore internet helpers.
    pub local_only: bool,
    /// Explicitly advertise this call to nearby devices using temporary identifiers.
    pub nearby_discovery: bool,
    pub rendezvous: Option<NodeName>,
    pub forwarder: Option<NodeName>,
    pub advertise: Vec<SocketAddr>,
    pub port: u16,
    /// Ask the router to open `port` while hosting (`router.rs`). Off unless chosen.
    pub open_port: bool,
    /// When joining, reach others only through the relay, so they never learn your address
    /// (`CallNode::set_relay_only`). Not used when hosting: an invite carries the host's
    /// addresses.
    pub relay_only: bool,
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
        config.enforce_policy()
    }

    /// Privacy policy wins over saved fields and environment overrides. It only ever removes
    /// reach: it never turns a privacy setting off. Hide my address stays on with Local only,
    /// and [`Self::conflict`] then stops the call instead of quietly dialling direct.
    pub fn enforce_policy(mut self) -> Self {
        if self.local_only {
            self.rendezvous = None;
            self.forwarder = None;
            self.advertise.clear();
            self.open_port = false;
        } else {
            // Nearby discovery hands the invitation to anyone on the network. Outside Local
            // only that invitation names internet addresses and helpers, so it stays off.
            self.nearby_discovery = false;
        }
        self
    }

    /// Why these settings cannot make a call, if they cannot. Checked before any socket opens.
    pub fn conflict(&self) -> Option<&'static str> {
        (self.local_only && self.relay_only).then_some(
            "Hide my address needs a relay, and Local only uses none. Turn one of them off in Settings > Connection.",
        )
    }
}

/// A node as `KEY@HOST:PORT`, the host an IP or a DNS name; blank is none. An IP must be one
/// a client can dial. A name is checked again when it is resolved (`NodeName::resolve`).
pub fn parse_node(text: &str) -> Result<Option<NodeName>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let node: NodeName = text
        .parse()
        .map_err(|_| String::from("Use KEY@IP:PORT as veil-node prints it, or KEY@NAME:PORT."))?;
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
///
/// The routing table knows which interface is the real network: it skips virtual switches
/// (Hyper-V, WSL, Docker) that sort first by address. Connecting a UDP socket only looks the
/// route up; nothing is sent. With no default route (a hotspot with no internet), the
/// interface list is used instead.
pub fn lan_ip() -> Option<IpAddr> {
    routed_ip().or_else(|| {
        let candidates: Vec<IpAddr> = veil_net::local::candidates(1)
            .into_iter()
            .map(|a| a.ip())
            .collect();
        fallback_ip(&candidates)
    })
}

fn routed_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A documentation address (TEST-NET-1): nothing is sent, the route is only looked up.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// With no route to go by: a private IPv4 address before a self-assigned (169.254) one,
/// which is what an adapter with no network gives itself.
///
/// shortcut: with several private networks and no default route, the lowest address wins
/// and may be a virtual switch. Upgrade by letting the person pick the network in
/// Connection if offline hotspots with virtual adapters turn out to be common.
fn fallback_ip(candidates: &[IpAddr]) -> Option<IpAddr> {
    let v4 = || {
        candidates
            .iter()
            .filter_map(|ip| match ip {
                IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(*v4),
                _ => None,
            })
    };
    v4().find(|ip| ip.is_private())
        .or_else(|| v4().next())
        .map(IpAddr::V4)
}

/// Addresses for the invite: configured ones first, then this machine's public IPv6 address
/// (`v6`, from `veil_net::family::global_ipv6`), then the LAN address, then this machine
/// itself (for a second copy of Veil on the same computer).
pub fn advertised(
    config: &NetConfig,
    v6: Option<Ipv6Addr>,
    lan: Option<IpAddr>,
    port: u16,
) -> Vec<SocketAddr> {
    advertised_with_candidates(config, v6, lan, port, &veil_net::local::local_candidates(port))
}

fn advertised_with_candidates(
    config: &NetConfig,
    v6: Option<Ipv6Addr>,
    lan: Option<IpAddr>,
    port: u16,
    candidates: &[SocketAddr],
) -> Vec<SocketAddr> {
    let mut addrs = if config.local_only {
        Vec::new()
    } else {
        config.advertise.clone()
    };
    if let Some(ip) = v6.filter(|ip| !config.local_only && veil_net::family::is_global_v6(*ip)) {
        addrs.push(SocketAddr::new(ip.into(), port));
    }
    if let Some(ip) = lan {
        addrs.push(SocketAddr::new(ip, port));
    }
    // Other interfaces only help guests on the same network, so they are listed only for
    // a Local only call, and only local scope ones. An invite that may travel over the
    // internet keeps the short list: configured, public IPv6, the LAN address, loopback.
    if config.local_only {
        addrs.extend(
            candidates
                .iter()
                .copied()
                .filter(|a| veil_net::local::is_local_scope(a.ip())),
        );
    }
    addrs.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port));
    let mut seen = std::collections::HashSet::new();
    addrs.retain(|addr| seen.insert(*addr));
    addrs.truncate(veil_security::invite::MAX_ADDRS);
    addrs
}

/// This epoch's plan and the directory behind it. Only the host's addresses are known, so in
/// a mesh members reach each other by punching through the rendezvous. With a `forwarder`, a
/// call larger than `Policy::mesh_max` goes through it instead.
#[cfg(test)]
pub fn plan_for(
    roster: &[(u32, PublicId)],
    me: PublicId,
    addrs: &[SocketAddr],
    forwarder: Option<NodeAddr>,
) -> Result<(Plan, Directory), String> {
    plan_for_with_addresses(roster, me, addrs, forwarder, &HashMap::new())
}

pub fn plan_for_with_addresses(
    roster: &[(u32, PublicId)],
    me: PublicId,
    addrs: &[SocketAddr],
    forwarder: Option<NodeAddr>,
    known: &HashMap<PublicId, Vec<SocketAddr>>,
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
                    known.get(device).cloned().unwrap_or_default()
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

    // Address policy tests must not depend on the test machine's actual interfaces.
    fn advertised(
        config: &NetConfig,
        v6: Option<Ipv6Addr>,
        lan: Option<IpAddr>,
        port: u16,
    ) -> Vec<SocketAddr> {
        super::advertised_with_candidates(config, v6, lan, port, &[])
    }

    #[test]
    fn local_policy_wins_over_environment_and_keeps_listening_port() {
        let config = NetConfig {
            local_only: true,
            open_port: true,
            relay_only: true,
            ..NetConfig::default()
        }
        .with_env(|name| match name {
            ENV_PORT => Some("7711".into()),
            ENV_ADVERTISE => Some("198.51.100.2:7710".into()),
            _ => None,
        });
        assert!(config.local_only);
        assert!(!config.open_port);
        assert!(config.relay_only, "Local only never turns Hide my address off");
        assert!(config.conflict().is_some(), "the pair stops the call instead");
        assert!(
            config.rendezvous.is_none()
                && config.forwarder.is_none()
                && config.advertise.is_empty()
        );
        assert_eq!(config.port, 7711);
    }

    #[test]
    fn without_a_route_a_private_address_beats_a_self_assigned_one() {
        let ips: Vec<IpAddr> = ["fd00::1", "169.254.120.25", "192.168.1.139", "127.0.0.1"]
            .iter()
            .map(|ip| ip.parse().unwrap())
            .collect();
        assert_eq!(super::fallback_ip(&ips), Some("192.168.1.139".parse().unwrap()));
        let only_link_local: Vec<IpAddr> = vec!["169.254.1.2".parse().unwrap()];
        assert_eq!(super::fallback_ip(&only_link_local), Some(only_link_local[0]));
        assert_eq!(super::fallback_ip(&["::1".parse().unwrap()]), None);
    }

    #[test]
    fn all_interface_candidates_are_deduplicated_and_bounded() {
        let a: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let b: SocketAddr = "10.20.0.2:5000".parse().unwrap();
        let public: SocketAddr = "81.2.69.160:5000".parse().unwrap();
        let local = NetConfig {
            local_only: true,
            ..NetConfig::default()
        };
        let list =
            super::advertised_with_candidates(&local, None, Some(a.ip()), 5000, &[a, b, a, public]);
        assert_eq!(list, vec![a, b, "127.0.0.1:5000".parse().unwrap()]);
        // An invite that may cross the internet lists no extra interfaces.
        let list = super::advertised_with_candidates(
            &NetConfig::default(),
            None,
            Some(a.ip()),
            5000,
            &[a, b, public],
        );
        assert_eq!(list, vec![a, "127.0.0.1:5000".parse().unwrap()]);
    }

    #[test]
    fn admitted_guest_addresses_are_included_in_mesh_routes() {
        let (host, guest) = (Identity::generate().public(), Identity::generate().public());
        let addr = "192.168.1.22:5001".parse().unwrap();
        let (_, directory) = plan_for_with_addresses(
            &[(0, host), (1, guest)],
            host,
            &[],
            None,
            &HashMap::from([(guest, vec![addr])]),
        )
        .unwrap();
        assert_eq!(directory.members[&NodeId(1)].addrs, vec![addr]);
    }

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
        let addrs = advertised(&config, None, Some(lan), 5000);
        let text: Vec<String> = addrs.iter().map(ToString::to_string).collect();
        assert_eq!(
            text,
            ["198.51.100.2:7710", "192.168.1.20:5000", "127.0.0.1:5000"]
        );
        assert_eq!(advertised(&NetConfig::default(), None, None, 1).len(), 1);
    }

    #[test]
    fn invites_offer_public_ipv6_after_configured_addresses() {
        let config = NetConfig {
            advertise: vec!["198.51.100.2:7710".parse().unwrap()],
            ..NetConfig::default()
        };
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let v6: Ipv6Addr = "2a00:23c5:1234:5600::1".parse().unwrap();
        let text: Vec<String> = advertised(&config, Some(v6), Some(lan), 5000)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            text,
            [
                "198.51.100.2:7710",
                "[2a00:23c5:1234:5600::1]:5000",
                "192.168.1.20:5000",
                "127.0.0.1:5000"
            ]
        );
        // A private (unique local) IPv6 address never goes in an invite.
        let ula: Ipv6Addr = "fd00::1".parse().unwrap();
        assert_eq!(
            advertised(&NetConfig::default(), Some(ula), None, 1).len(),
            1
        );
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
