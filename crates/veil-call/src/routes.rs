//! Turns a `veil-topology` plan into the media links one participant needs.
//!
//! Purpose: the planner speaks in [`NodeId`]s. A [`Directory`] maps each id to a member's
//! device key (and any direct addresses known for it) or to a forwarder's [`NodeAddr`].
//! [`routes`] then says what this participant connects to: every other member (mesh), or its
//! home forwarder (forwarded).
//!
//! Invariants: pure; an id the plan names but the directory lacks is an error, never a guess.
//!
//! In a call only the host plans: it sends a signed [`crate::CallPlan`] on every epoch change,
//! and members take their routes from that. The types here are what that plan resolves to.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use veil_net::NodeAddr;
use veil_security::PublicId;
use veil_topology::NodeId;

/// A member as the media layer sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub device: PublicId,
    /// Direct addresses to try before rendezvous, if any are known.
    pub addrs: Vec<SocketAddr>,
}

/// Who each planner id is, for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    /// This participant's own id.
    pub me: NodeId,
    /// Every participant, including this one.
    pub members: BTreeMap<NodeId, Peer>,
    /// Every node that may forward, as a reachable forwarder address.
    pub forwarders: BTreeMap<NodeId, NodeAddr>,
}

/// The media links one participant should hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routes {
    /// A link to each of these members. `reachable`: the plan lists a direct address for this
    /// participant, so [`dials`] lets members without one dial it.
    Mesh { peers: Vec<Peer>, reachable: bool },
    /// One link, to this forwarder, admitted with the host's sealed roster ticket.
    Forwarder { addr: NodeAddr, ticket: Vec<u8> },
}

/// Whether this participant (key `me`, `reachable` if the plan lists an address for it) dials
/// `other`, rather than waiting for `other` to dial it. Both ends read the same plan, so
/// exactly one of them dials.
///
/// The one without a known address dials the one with: only the host's addresses are known,
/// so a guest behind a router that opens no port can still reach a host whose port is open.
/// When both or neither have one, the lower key dials.
pub fn dials(me: &PublicId, reachable: bool, other: &Peer) -> bool {
    match (reachable, other.addrs.is_empty()) {
        (false, false) => true,
        (true, true) => false,
        _ => me.0 < other.device.0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use veil_security::Identity;
    use veil_topology::{Node, NodeKind, Plan, Policy, plan};

    use super::*;
    use crate::{CallError, CallPlan};

    /// What the host's plan gives `directory.me`.
    fn routes(plan: &Plan, directory: &Directory) -> Result<Routes, CallError> {
        let me = directory
            .members
            .get(&directory.me)
            .ok_or(CallError::Plan("this participant is not in the directory"))?
            .device;
        CallPlan::from_plan(plan, directory, [0; 16], 0, &Identity::generate())?.routes_for(&me)
    }

    fn node(id: u32, kind: NodeKind, participant: bool, kbps: u64) -> Node {
        Node {
            id: NodeId(id),
            kind,
            uplink_kbps: kbps,
            downlink_kbps: kbps,
            participant,
            can_forward: !participant,
        }
    }

    fn directory(me: u32, members: u32, forwarder: Option<u32>) -> Directory {
        Directory {
            me: NodeId(me),
            members: (0..members)
                .map(|i| {
                    let peer = Peer {
                        device: PublicId([u8::try_from(i).unwrap(); 32]),
                        addrs: Vec::new(),
                    };
                    (NodeId(i), peer)
                })
                .collect(),
            forwarders: forwarder
                .map(|f| {
                    let addr = NodeAddr {
                        key: PublicId([0xF0; 32]),
                        addr: "192.0.2.9:4433".parse().unwrap(),
                    };
                    (NodeId(f), addr)
                })
                .into_iter()
                .collect(),
        }
    }

    fn peer(byte: u8, reachable: bool) -> Peer {
        let addrs = if reachable {
            vec!["192.0.2.1:7710".parse().unwrap()]
        } else {
            Vec::new()
        };
        Peer {
            device: PublicId([byte; 32]),
            addrs,
        }
    }

    #[test]
    fn exactly_one_side_of_every_pair_dials() {
        for (a, b) in [(1, 2), (2, 1)] {
            for (ra, rb) in [(false, false), (true, false), (false, true), (true, true)] {
                let (pa, pb) = (peer(a, ra), peer(b, rb));
                let a_dials = dials(&pa.device, ra, &pb);
                let b_dials = dials(&pb.device, rb, &pa);
                assert!(a_dials != b_dials, "keys {a},{b} reachable {ra},{rb}");
            }
        }
    }

    #[test]
    fn a_guest_dials_a_reachable_host_whatever_the_keys() {
        // The host's key is lower, which used to make the host dial a guest it cannot reach.
        let (host, guest) = (peer(1, true), peer(9, false));
        assert!(dials(&guest.device, false, &host));
        assert!(!dials(&host.device, true, &guest));
    }

    #[test]
    fn mesh_links_every_other_member() {
        let nodes: Vec<Node> = (0..3).map(|i| node(i, NodeKind::Peer, true, 100_000)).collect();
        let plan = plan(&nodes, &[], &Policy::default()).unwrap();
        let Routes::Mesh { peers, .. } = routes(&plan, &directory(1, 3, None)).unwrap() else {
            panic!("small call should mesh");
        };
        let keys: Vec<u8> = peers.iter().map(|p| p.device.0[0]).collect();
        assert_eq!(keys, vec![0, 2]);
    }

    #[test]
    fn forwarded_plan_names_the_home_forwarder() {
        let mut nodes: Vec<Node> = (0..3).map(|i| node(i, NodeKind::Peer, true, 100_000)).collect();
        nodes.push(node(9, NodeKind::SelfHosted, false, 1_000_000));
        let policy = Policy {
            mesh_max: 1,
            ..Policy::default()
        };
        let plan = plan(&nodes, &[], &policy).unwrap();
        let routed = routes(&plan, &directory(0, 3, Some(9))).unwrap();
        assert!(matches!(routed, Routes::Forwarder { addr, .. } if addr.key == PublicId([0xF0; 32])));
        assert!(matches!(
            routes(&plan, &directory(0, 3, None)),
            Err(CallError::Plan(_))
        ));
    }

    #[test]
    fn a_participant_missing_from_the_directory_is_refused() {
        let nodes: Vec<Node> = (0..2).map(|i| node(i, NodeKind::Peer, true, 100_000)).collect();
        let plan = plan(&nodes, &[], &Policy::default()).unwrap();
        assert!(matches!(
            routes(&plan, &directory(5, 2, None)),
            Err(CallError::Plan(_))
        ));
    }
}
