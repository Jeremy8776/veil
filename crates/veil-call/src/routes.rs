//! Turns a `veil-topology` plan into the media links one participant needs.
//!
//! Purpose: the planner speaks in [`NodeId`]s. A [`Directory`] maps each id to a member's
//! device key (and any direct addresses known for it) or to a forwarder's [`NodeAddr`].
//! [`routes`] then says what this participant connects to: every other member (mesh), or its
//! home forwarder (forwarded).
//!
//! Invariants: pure; an id the plan names but the directory lacks is an error, never a guess.
//!
//! shortcut: every participant plans from its own inputs, so two members with different
//! bandwidth figures could choose different layouts. Upgrade by having the host sign and send
//! the plan over the control link.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use veil_net::NodeAddr;
use veil_security::PublicId;
use veil_topology::{NodeId, Plan};

use crate::CallError;

/// A member as the media layer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A link to each of these members.
    Mesh(Vec<Peer>),
    /// One link, to this forwarder.
    Forwarder(NodeAddr),
}

pub fn routes(plan: &Plan, directory: &Directory) -> Result<Routes, CallError> {
    if !directory.members.contains_key(&directory.me) {
        return Err(CallError::Plan("this participant is not in the directory"));
    }
    match plan {
        Plan::Mesh { .. } => Ok(Routes::Mesh(
            directory
                .members
                .iter()
                .filter(|(id, _)| **id != directory.me)
                .map(|(_, peer)| peer.clone())
                .collect(),
        )),
        Plan::Forwarded {
            forwarders, home, ..
        } => {
            // shortcut: veil-forward has no forwarder-to-forwarder cascade, so a plan with
            // more than one forwarder cannot be carried yet. Upgrade with the cascade.
            if forwarders.len() > 1 {
                return Err(CallError::Plan("cascaded forwarders are not supported yet"));
            }
            let mine = home
                .get(&directory.me)
                .ok_or(CallError::Plan("the plan gives this participant no forwarder"))?;
            directory
                .forwarders
                .get(mine)
                .copied()
                .map(Routes::Forwarder)
                .ok_or(CallError::Plan("the plan's forwarder is not in the directory"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use veil_topology::{Node, NodeKind, Policy, plan};

    use super::*;

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

    #[test]
    fn mesh_links_every_other_member() {
        let nodes: Vec<Node> = (0..3).map(|i| node(i, NodeKind::Peer, true, 100_000)).collect();
        let plan = plan(&nodes, &[], &Policy::default()).unwrap();
        let Routes::Mesh(peers) = routes(&plan, &directory(1, 3, None)).unwrap() else {
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
        assert!(matches!(routed, Routes::Forwarder(addr) if addr.key == PublicId([0xF0; 32])));
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
