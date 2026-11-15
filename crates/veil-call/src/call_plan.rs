//! The host's signed call plan: one layout that every member follows.
//!
//! Purpose: the host turns a `veil-topology` [`Plan`] and a [`Directory`] into a [`CallPlan`]
//! for the current epoch ([`CallPlan::from_plan`]), signs it ([`CallPlan::seal`]) and sends it
//! on every member's control link. Each member checks it with [`CallPlan::open`] and takes its
//! own [`Routes`] with [`CallPlan::routes_for`]. Every member follows the host's one layout, so
//! two members can never choose different layouts from different bandwidth figures.
//!
//! Invariants:
//! - A plan names device keys and addresses, never planner ids, so a member needs no directory.
//! - The signature covers a domain tag and the encoded plan, which holds the room id and epoch,
//!   so a plan cannot be replayed into another call or another epoch.
//! - Decoding is bounded: at most [`MAX_CONTROL_LEN`] bytes, [`MAX_MEMBERS`] members and
//!   [`MAX_PEER_ADDRS`] addresses per member, no member listed twice.
//! - A forwarded plan carries the host's sealed [`RosterTicket`] for the epoch, listing every
//!   member. Members show it to the forwarder, which admits no one else.
//! - Pure: no I/O.
//!
//! shortcut: a plan carries at most [`MAX_MEMBERS`] members so it fits one control message.
//! Upgrade by splitting the plan across messages once calls grow past that.

use std::collections::HashSet;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use veil_net::NodeAddr;
use veil_forward::RosterTicket;
use veil_net::control::MAX_CONTROL_LEN;
use veil_rendezvous::room_tag;
use veil_security::{Identity, PublicId};
use veil_topology::Plan;

use crate::CallError;
use crate::routes::{Directory, Peer, Routes};

pub const MAX_MEMBERS: usize = 128;
pub const MAX_PEER_ADDRS: usize = 8;
const SIGNATURE_LEN: usize = 64;
const DOMAIN: &[u8] = b"veil-call-plan-v1";

/// How the members link up.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// Every member links to every other member.
    Mesh,
    /// Every member links to this one forwarder and shows it `ticket`, a sealed
    /// [`RosterTicket`].
    Forwarded {
        key: PublicId,
        addr: SocketAddr,
        ticket: Vec<u8>,
    },
}

/// One epoch's layout, as the host decided it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CallPlan {
    pub room_id: [u8; 16],
    pub epoch: u64,
    pub layout: Layout,
    /// Every member in this epoch, including the host.
    pub members: Vec<Peer>,
}

impl CallPlan {
    /// Resolves a planner's output into devices and addresses for one epoch. `host` signs the
    /// forwarder's roster ticket.
    pub fn from_plan(
        plan: &Plan,
        directory: &Directory,
        room_id: [u8; 16],
        epoch: u64,
        host: &Identity,
    ) -> Result<Self, CallError> {
        let layout = match plan {
            Plan::Mesh { .. } => Layout::Mesh,
            Plan::Forwarded {
                forwarders, home, ..
            } => {
                // shortcut: veil-forward has no forwarder-to-forwarder cascade, so a plan with
                // more than one forwarder cannot be carried yet. Upgrade with the cascade.
                let [forwarder] = forwarders.as_slice() else {
                    return Err(CallError::Plan("cascaded forwarders are not supported yet"));
                };
                if directory.members.keys().any(|id| home.get(id) != Some(forwarder)) {
                    return Err(CallError::Plan("the plan gives a participant no forwarder"));
                }
                let addr = directory
                    .forwarders
                    .get(forwarder)
                    .ok_or(CallError::Plan("the plan's forwarder is not in the directory"))?;
                let ticket = RosterTicket {
                    room_tag: room_tag(&room_id),
                    host: host.public(),
                    epoch,
                    members: directory.members.values().map(|m| m.device).collect(),
                };
                Layout::Forwarded {
                    key: addr.key,
                    addr: addr.addr,
                    ticket: ticket
                        .seal(host)
                        .map_err(|_| CallError::Plan("the roster ticket cannot be signed"))?,
                }
            }
        };
        let plan = Self {
            room_id,
            epoch,
            layout,
            members: directory.members.values().cloned().collect(),
        };
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> Result<(), CallError> {
        if self.members.len() > MAX_MEMBERS {
            return Err(CallError::Plan("too many members for one plan"));
        }
        if self.members.iter().any(|m| m.addrs.len() > MAX_PEER_ADDRS) {
            return Err(CallError::Plan("too many addresses for one member"));
        }
        let mut seen = HashSet::new();
        if !self.members.iter().all(|m| seen.insert(m.device)) {
            return Err(CallError::Plan("a member is listed twice"));
        }
        Ok(())
    }

    /// The links `me` should hold under this plan.
    pub fn routes_for(&self, me: &PublicId) -> Result<Routes, CallError> {
        if !self.members.iter().any(|m| m.device == *me) {
            return Err(CallError::Plan("this participant is not in the plan"));
        }
        Ok(match &self.layout {
            Layout::Mesh => Routes::Mesh(
                self.members
                    .iter()
                    .filter(|m| m.device != *me)
                    .cloned()
                    .collect(),
            ),
            Layout::Forwarded { key, addr, ticket } => Routes::Forwarder {
                addr: NodeAddr { key: *key, addr: *addr },
                ticket: ticket.clone(),
            },
        })
    }

    /// True when the plan lists exactly these devices.
    pub fn covers(&self, devices: &[PublicId]) -> bool {
        let listed: HashSet<PublicId> = self.members.iter().map(|m| m.device).collect();
        listed.len() == devices.len() && devices.iter().all(|d| listed.contains(d))
    }

    /// Encodes and signs the plan. The result rides in `Control::Plan`.
    pub fn seal(&self, host: &Identity) -> Result<Vec<u8>, CallError> {
        self.validate()?;
        let mut bytes =
            postcard::to_allocvec(self).map_err(|_| CallError::Plan("the plan does not encode"))?;
        let signature = host.sign(&signed_message(&bytes));
        bytes.extend_from_slice(&signature);
        if bytes.len() > MAX_CONTROL_LEN {
            return Err(CallError::Plan("the plan is too large"));
        }
        Ok(bytes)
    }

    /// Checks a sealed plan: signed by `host`, for this room and this epoch.
    pub fn open(
        sealed: &[u8],
        host: &PublicId,
        room_id: [u8; 16],
        epoch: u64,
    ) -> Result<Self, CallError> {
        if sealed.len() > MAX_CONTROL_LEN {
            return Err(CallError::Plan("the plan is too large"));
        }
        let split = sealed
            .len()
            .checked_sub(SIGNATURE_LEN)
            .filter(|n| *n > 0)
            .ok_or(CallError::Protocol("a signed plan"))?;
        let (encoded, signature) = sealed.split_at(split);
        let signature: [u8; SIGNATURE_LEN] = signature
            .try_into()
            .map_err(|_| CallError::Protocol("a signed plan"))?;
        host.verify(&signed_message(encoded), &signature)
            .map_err(|_| CallError::Protocol("a plan signed by the host"))?;
        let plan: Self =
            postcard::from_bytes(encoded).map_err(|_| CallError::Protocol("a signed plan"))?;
        plan.validate()?;
        if plan.room_id != room_id {
            return Err(CallError::Plan("the plan is for another call"));
        }
        if plan.epoch != epoch {
            return Err(CallError::Plan("the plan is for another epoch"));
        }
        Ok(plan)
    }
}

fn signed_message(encoded: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + encoded.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(encoded);
    message
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use veil_topology::NodeId;

    use super::*;

    const ROOM: [u8; 16] = [3; 16];

    fn sample(members: u8) -> CallPlan {
        CallPlan {
            room_id: ROOM,
            epoch: 4,
            layout: Layout::Mesh,
            members: (0..members)
                .map(|i| Peer {
                    device: PublicId([i; 32]),
                    addrs: vec!["192.0.2.1:4433".parse().unwrap()],
                })
                .collect(),
        }
    }

    #[test]
    fn a_sealed_plan_opens_for_its_room_and_epoch() {
        let host = Identity::generate();
        let sealed = sample(3).seal(&host).unwrap();
        let opened = CallPlan::open(&sealed, &host.public(), ROOM, 4).unwrap();
        assert_eq!(opened, sample(3));
        let Routes::Mesh(peers) = opened.routes_for(&PublicId([1; 32])).unwrap() else {
            panic!("mesh plan");
        };
        assert_eq!(peers.len(), 2);
    }

    #[test]
    fn a_plan_from_anyone_else_or_for_another_call_is_refused() {
        let host = Identity::generate();
        let sealed = sample(2).seal(&host).unwrap();
        let stranger = Identity::generate().public();
        assert!(matches!(
            CallPlan::open(&sealed, &stranger, ROOM, 4),
            Err(CallError::Protocol(_))
        ));
        assert!(CallPlan::open(&sealed, &host.public(), [9; 16], 4).is_err());
        assert!(CallPlan::open(&sealed, &host.public(), ROOM, 5).is_err());

        let mut tampered = sealed.clone();
        tampered[0] ^= 1;
        assert!(CallPlan::open(&tampered, &host.public(), ROOM, 4).is_err());
        assert!(CallPlan::open(&sealed[..10], &host.public(), ROOM, 4).is_err());
        assert!(CallPlan::open(&[], &host.public(), ROOM, 4).is_err());
    }

    #[test]
    fn oversized_or_duplicate_member_lists_are_refused() {
        let host = Identity::generate();
        let too_many = u8::try_from(MAX_MEMBERS + 1).unwrap();
        assert!(sample(too_many).seal(&host).is_err());
        let mut twice = sample(2);
        twice.members.push(twice.members[0].clone());
        assert!(twice.seal(&host).is_err());
    }

    #[test]
    fn a_participant_outside_the_plan_gets_no_routes() {
        assert!(sample(2).routes_for(&PublicId([7; 32])).is_err());
    }

    #[test]
    fn covers_checks_the_exact_member_set() {
        let plan = sample(2);
        assert!(plan.covers(&[PublicId([1; 32]), PublicId([0; 32])]));
        assert!(!plan.covers(&[PublicId([0; 32])]));
        assert!(!plan.covers(&[PublicId([0; 32]), PublicId([1; 32]), PublicId([2; 32])]));
    }

    #[test]
    fn a_forwarded_plan_needs_every_member_homed() {
        let directory = Directory {
            me: NodeId(0),
            members: (0..2u8)
                .map(|i| (NodeId(i.into()), Peer { device: PublicId([i; 32]), addrs: Vec::new() }))
                .collect(),
            forwarders: BTreeMap::from([(
                NodeId(9),
                NodeAddr { key: PublicId([0xF0; 32]), addr: "192.0.2.9:4433".parse().unwrap() },
            )]),
        };
        let homed = |ids: &[u32]| Plan::Forwarded {
            forwarders: vec![NodeId(9)],
            home: ids.iter().map(|i| (NodeId(*i), NodeId(9))).collect(),
            subscriptions: Vec::new(),
            degraded: false,
        };
        let host = Identity::generate();
        let plan = CallPlan::from_plan(&homed(&[0, 1]), &directory, ROOM, 1, &host).unwrap();
        let Layout::Forwarded { key, ticket, .. } = &plan.layout else {
            panic!("forwarded plan");
        };
        assert_eq!(*key, PublicId([0xF0; 32]));
        let ticket = RosterTicket::open(ticket).unwrap();
        assert_eq!((ticket.host, ticket.epoch), (host.public(), 1));
        assert_eq!(ticket.room_tag, room_tag(&ROOM));
        assert!(ticket.lists(&PublicId([0; 32])) && ticket.lists(&PublicId([1; 32])));
        assert!(CallPlan::from_plan(&homed(&[0]), &directory, ROOM, 1, &host).is_err());
    }
}
