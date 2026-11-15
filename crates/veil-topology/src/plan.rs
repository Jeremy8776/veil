//! The planning algorithm. See the crate docs for the strategy.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::{AUDIO_KBPS, Layer, MAX_NODES, Node, NodeId, Plan, PlanError, Policy, Subscription};

type Subs = BTreeMap<(NodeId, NodeId), Layer>;

struct Ctx<'a> {
    nodes: BTreeMap<NodeId, &'a Node>,
    participants: Vec<NodeId>,
    policy: &'a Policy,
}

impl Ctx<'_> {
    fn usable(&self, kbps: u64) -> u64 {
        kbps.saturating_mul(self.policy.headroom_percent.clamp(1, 100)) / 100
    }

    fn others(&self) -> u64 {
        (self.participants.len() as u64).saturating_sub(1)
    }

    /// Everything `r` downloads: audio from everyone else plus its video subscriptions.
    fn receive_load(&self, subs: &Subs, r: NodeId) -> u64 {
        let video: u64 = subs
            .iter()
            .filter(|((rx, _), _)| *rx == r)
            .map(|(_, l)| l.kbps())
            .sum();
        AUDIO_KBPS.saturating_mul(self.others()).saturating_add(video)
    }

    /// What `s` uploads in a mesh: one copy per receiver.
    fn mesh_upload(&self, subs: &Subs, s: NodeId) -> u64 {
        let video: u64 = subs
            .iter()
            .filter(|((_, tx), _)| *tx == s)
            .map(|(_, l)| l.kbps())
            .sum();
        AUDIO_KBPS.saturating_mul(self.others()).saturating_add(video)
    }

    /// What `s` uploads to a forwarder: each requested layer once (layer pausing drops the rest).
    fn published(&self, subs: &Subs, s: NodeId) -> u64 {
        let layers: BTreeSet<Layer> = subs
            .iter()
            .filter(|((_, tx), _)| *tx == s)
            .map(|(_, l)| *l)
            .collect();
        AUDIO_KBPS.saturating_add(layers.iter().map(|l| l.kbps()).sum())
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id).copied()
    }

    fn fits(&self, id: NodeId, up: u64, down: u64) -> bool {
        self.node(id).is_some_and(|n| {
            up <= self.usable(n.uplink_kbps) && down <= self.usable(n.downlink_kbps)
        })
    }

    fn mesh_fits(&self, subs: &Subs) -> bool {
        self.participants.len() <= self.policy.mesh_max
            && self.participants.iter().all(|p| {
                self.fits(*p, self.mesh_upload(subs, *p), self.receive_load(subs, *p))
            })
    }

    fn candidates(&self) -> Vec<NodeId> {
        let prefs = &self.policy.forwarder_preference;
        let mut list: Vec<(usize, Reverse<u64>, NodeId)> = self
            .nodes
            .values()
            .filter(|n| n.can_forward)
            .filter_map(|n| {
                let rank = prefs.iter().position(|k| *k == n.kind)?;
                Some((rank, Reverse(n.uplink_kbps), n.id))
            })
            .collect();
        list.sort();
        list.into_iter().map(|(_, _, id)| id).collect()
    }

    fn forwarded_fit(&self, subs: &Subs) -> Option<(Vec<NodeId>, BTreeMap<NodeId, NodeId>)> {
        let candidates = self.candidates();
        let limit = self.policy.max_forwarders.min(candidates.len());
        // Participants that do not forward must fit on their own links whatever the layout.
        for f in 1..=limit {
            let forwarders = &candidates[..f];
            let home = self.assign(subs, forwarders);
            if self.forwarders_fit(subs, forwarders, &home)
                && self.participants.iter().all(|p| {
                    forwarders.contains(p)
                        || self.fits(*p, self.published(subs, *p), self.receive_load(subs, *p))
                })
            {
                return Some((forwarders.to_vec(), home));
            }
        }
        None
    }

    /// Forwarders that are participants serve themselves. Everyone else goes, heaviest first,
    /// to the forwarder with the most spare upload.
    fn assign(&self, subs: &Subs, forwarders: &[NodeId]) -> BTreeMap<NodeId, NodeId> {
        let mut home = BTreeMap::new();
        let mut spare: BTreeMap<NodeId, u64> = forwarders
            .iter()
            .map(|f| (*f, self.node(*f).map_or(0, |n| self.usable(n.uplink_kbps))))
            .collect();
        let mut rest: Vec<(Reverse<u64>, NodeId)> = Vec::new();
        for p in &self.participants {
            if forwarders.contains(p) {
                home.insert(*p, *p);
            } else {
                rest.push((Reverse(self.receive_load(subs, *p)), *p));
            }
        }
        rest.sort();
        for (Reverse(load), p) in rest {
            // Most spare first; ties go to the earlier (more preferred) forwarder.
            let best = forwarders
                .iter()
                .max_by_key(|f| (spare.get(f).copied().unwrap_or(0), Reverse(**f)))
                .copied();
            if let Some(f) = best {
                if let Some(s) = spare.get_mut(&f) {
                    *s = s.saturating_sub(load);
                }
                home.insert(p, f);
            }
        }
        home
    }

    fn forwarders_fit(
        &self,
        subs: &Subs,
        forwarders: &[NodeId],
        home: &BTreeMap<NodeId, NodeId>,
    ) -> bool {
        let total: u64 = self.participants.iter().map(|p| self.published(subs, *p)).sum();
        let peers = forwarders.len().saturating_sub(1) as u64;
        forwarders.iter().all(|f| {
            let local: Vec<NodeId> = home
                .iter()
                .filter(|(_, h)| *h == f)
                .map(|(p, _)| *p)
                .collect();
            let local_published: u64 = local.iter().map(|p| self.published(subs, *p)).sum();
            let own = if local.contains(f) { self.published(subs, *f) } else { 0 };
            let to_receivers: u64 = local
                .iter()
                .filter(|p| *p != f)
                .map(|p| self.receive_load(subs, *p))
                .sum();
            // shortcut: every forwarder receives every stream, even layers none of its receivers
            // want. Upgrade to per-forwarder layer filtering when cascades get large.
            let up = to_receivers.saturating_add(peers.saturating_mul(local_published));
            let down = total.saturating_sub(own);
            self.fits(*f, up, down)
        })
    }
}

/// Lowers every subscription at the highest layer in use. Returns false when all are Thumb.
fn degrade(subs: &mut Subs) -> bool {
    let Some(top) = subs.values().max().copied() else {
        return false;
    };
    if top == Layer::Thumb {
        return false;
    }
    for layer in subs.values_mut() {
        if *layer == top {
            *layer = top.lower();
        }
    }
    true
}

fn to_vec(subs: &Subs) -> Vec<Subscription> {
    subs.iter()
        .map(|((receiver, sender), layer)| Subscription {
            receiver: *receiver,
            sender: *sender,
            layer: *layer,
        })
        .collect()
}

/// Chooses a layout. See the crate docs.
pub fn plan(nodes: &[Node], subscriptions: &[Subscription], policy: &Policy) -> Result<Plan, PlanError> {
    if nodes.len() > MAX_NODES || subscriptions.len() > MAX_NODES.saturating_mul(MAX_NODES) {
        return Err(PlanError::TooManyNodes);
    }
    let mut map = BTreeMap::new();
    for n in nodes {
        if map.insert(n.id, n).is_some() {
            return Err(PlanError::DuplicateNode(n.id));
        }
    }
    let participants: Vec<NodeId> = nodes.iter().filter(|n| n.participant).map(|n| n.id).collect();
    let ctx = Ctx {
        nodes: map,
        participants,
        policy,
    };

    let mut subs = Subs::new();
    for s in subscriptions {
        for id in [s.receiver, s.sender] {
            let node = ctx.node(id).ok_or(PlanError::UnknownNode(id))?;
            if !node.participant {
                return Err(PlanError::NotParticipant(id));
            }
        }
        if s.receiver == s.sender {
            return Err(PlanError::SelfSubscription(s.receiver));
        }
        let slot = subs.entry((s.receiver, s.sender)).or_insert(s.layer);
        *slot = (*slot).max(s.layer);
    }

    if ctx.participants.len() < 2 {
        return Ok(Plan::Mesh {
            subscriptions: to_vec(&subs),
            degraded: false,
        });
    }

    // At most three rounds: as asked, then Uhd lowered, then Hd lowered.
    for round in 0..3 {
        let degraded = round > 0;
        if ctx.mesh_fits(&subs) {
            return Ok(Plan::Mesh {
                subscriptions: to_vec(&subs),
                degraded,
            });
        }
        if let Some((forwarders, home)) = ctx.forwarded_fit(&subs) {
            return Ok(Plan::Forwarded {
                forwarders,
                home,
                subscriptions: to_vec(&subs),
                degraded,
            });
        }
        if !degrade(&mut subs) {
            break;
        }
    }
    Err(PlanError::Infeasible)
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
