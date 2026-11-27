//! The leaving host's side of a planned handover in a persistent call (ADR 0010 §5 to §8).
//!
//! Purpose: collect members' coarse availability reports, offer hosting to the best one (or
//! the one the host picked), and on a signed `HandoverReady` commit the handover, tell every
//! member where the new host is, and step aside.
//!
//! Sequence: [`HostHandle::hand_over`] -> `HandoverOffer` to one successor -> `HandoverReady`
//! within [`READY_WAIT`] (or the next in order, at most [`MAX_ATTEMPTS`]) -> handover commit
//! and `HandedOver` to every member -> wait for the new host's removal commit or
//! [`LINGER_AFTER`] -> [`HostEvent::HandedOver`] and the actor stops.
//!
//! Invariants:
//! - Only a member that reported itself available, and whose link is not relayed, is offered.
//! - Nothing is committed until the successor's contact verifies under its own key for the
//!   next epoch. Before the commit, every failure leaves this device the host.
//! - After the commit this device is an ordinary member: it refuses lobby joiners and resumes
//!   with `HostChanged`, publishes no plan and never sends `Ended`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use veil_group::Applied;
use veil_net::control::{Availability, Control, MAX_HANDOVER_REMOVED, RefuseReason};
use veil_security::PublicId;

use super::contact::Contact;
use super::successor::{Bar, Candidate, MAX_ATTEMPTS, bar, rank};
use super::{Actor, HostEvent};

/// How long a successor has to start its host roles and answer.
pub const READY_WAIT: Duration = Duration::from_secs(10);
/// How long the old host waits for the new host's removal commit before it closes.
pub const LINGER_AFTER: Duration = Duration::from_secs(10);
/// The longest a handover can take, from [`HostHandle::hand_over`](super::HostHandle::hand_over)
/// to [`HostEvent::HandedOver`]: every successor tried runs out its ready time, then the old
/// host lingers. A caller that must not cut a handover short (quitting) waits this long.
pub const HANDOVER_MAX: Duration =
    Duration::from_secs(READY_WAIT.as_secs() * MAX_ATTEMPTS as u64 + LINGER_AFTER.as_secs());

/// One person the leaving host may pick, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub device: PublicId,
    pub bar: Bar,
}

/// Why a handover did not happen. The caller is still the host in every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoverFailure {
    /// The call is not persistent: leaving ends it.
    NotPersistent,
    /// Nobody is available to take over.
    NoOneAvailable,
    /// Every successor tried failed to get ready.
    NoneReady,
    /// A handover is already under way.
    Busy,
    /// The session stopped.
    Closed,
}

type Reply = oneshot::Sender<Result<PublicId, HandoverFailure>>;

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    Offering {
        order: VecDeque<PublicId>,
        current: PublicId,
        request: u64,
        until: Instant,
        reply: Reply,
    },
    /// Committed. Waiting for the new host's removal of this device.
    Handed { successor: PublicId, until: Instant },
    Done(PublicId),
}

/// The host actor's handover state.
#[derive(Default)]
pub(super) struct Handover {
    reports: HashMap<PublicId, Availability>,
    /// Members whose control link reached this host through a relay.
    relayed: HashSet<PublicId>,
    /// Devices removed or declined in this call, newest last, for the new host's lobby.
    removed: VecDeque<PublicId>,
    stage: Stage,
    next_request: u64,
}

impl Handover {
    pub(super) fn note_path(&mut self, device: PublicId, relayed: bool) {
        if relayed {
            self.relayed.insert(device);
        } else {
            self.relayed.remove(&device);
        }
    }

    pub(super) fn note_removed(&mut self, device: PublicId) {
        self.removed.retain(|d| *d != device);
        self.removed.push_back(device);
        while self.removed.len() > MAX_HANDOVER_REMOVED {
            self.removed.pop_front();
        }
    }

    pub(super) fn removed_earlier(&self, device: &PublicId) -> bool {
        self.removed.contains(device)
    }

    pub(super) fn forget(&mut self, device: &PublicId) {
        self.reports.remove(device);
        self.relayed.remove(device);
    }

    /// True once this device handed the call over and committed it.
    pub(super) fn stepped_aside(&self) -> bool {
        matches!(self.stage, Stage::Handed { .. } | Stage::Done(_))
    }

    pub(super) fn finished(&self) -> Option<PublicId> {
        match self.stage {
            Stage::Done(successor) => Some(successor),
            _ => None,
        }
    }
}

impl Actor {
    /// A successor's first state: the commit that removed the old host in the journal, every
    /// other member recovering until it resumes here, and the old host's removed list.
    pub(super) fn seed_take_over(&mut self, journal: Vec<(u64, Vec<u8>)>, removed: Vec<PublicId>) {
        self.history.extend(journal);
        let until = Instant::now() + super::FOLLOW_WINDOW;
        for device in self.others() {
            // Link id 0 is never issued, so no stale closure matches it.
            self.recovering.insert(device, (0, until));
        }
        for device in removed {
            self.handover.note_removed(device);
        }
    }

    fn candidates(&self) -> Vec<Candidate> {
        let me = self.identity.public();
        self.group
            .members()
            .into_iter()
            .map(|(_, device)| device)
            .filter(|device| *device != me)
            .filter_map(|device| {
                self.handover.reports.get(&device).map(|report| Candidate {
                    device,
                    report: *report,
                    relay_only: self.handover.relayed.contains(&device),
                })
            })
            .collect()
    }

    /// Available members with their bars, best first.
    pub(super) fn choices(&self) -> Vec<Choice> {
        let candidates = self.candidates();
        rank(&candidates)
            .into_iter()
            .filter_map(|device| {
                let candidate = candidates.iter().find(|c| c.device == device)?;
                Some(Choice {
                    device,
                    bar: bar(candidate)?,
                })
            })
            .collect()
    }

    /// A member's own report. Only members' reports count, and only within bounds.
    pub(super) fn availability(&mut self, device: PublicId, report: Availability) {
        let message = Control::Availability(report);
        if message.within_bounds() && self.group.members().iter().any(|(_, d)| *d == device) {
            self.handover.reports.insert(device, report);
        }
    }

    /// Starts a handover. `pick` moves the host's chosen person to the front; `None` is
    /// Automatic. Answers once committed, or when nobody could take over.
    pub(super) fn start_handover(&mut self, pick: Option<PublicId>, reply: Reply) {
        if !self.group.is_persistent() || !self.group.is_host() {
            let _ = reply.send(Err(HandoverFailure::NotPersistent));
            return;
        }
        if !matches!(self.handover.stage, Stage::Idle) {
            let _ = reply.send(Err(HandoverFailure::Busy));
            return;
        }
        let mut order: VecDeque<PublicId> =
            self.choices().into_iter().map(|c| c.device).collect();
        if let Some(pick) = pick
            && let Some(at) = order.iter().position(|d| *d == pick)
            && let Some(picked) = order.remove(at)
        {
            order.push_front(picked);
        }
        order.truncate(MAX_ATTEMPTS);
        self.offer_next(order, reply);
    }

    fn offer_next(&mut self, mut order: VecDeque<PublicId>, reply: Reply) {
        // A successor that dropped its link since it reported is skipped.
        while let Some(current) = order.pop_front() {
            if !self.links.contains_key(&current) {
                continue;
            }
            self.handover.next_request = self.handover.next_request.wrapping_add(1);
            let request = self.handover.next_request;
            let epoch = self.group.epoch();
            self.send_to(&current, Control::HandoverOffer { request, epoch });
            self.handover.stage = Stage::Offering {
                order,
                current,
                request,
                until: Instant::now() + READY_WAIT,
                reply,
            };
            return;
        }
        self.handover.stage = Stage::Idle;
        let failure = if self.choices().is_empty() {
            HandoverFailure::NoOneAvailable
        } else {
            HandoverFailure::NoneReady
        };
        let _ = reply.send(Err(failure));
    }

    /// Moves on to the next successor when the current one failed.
    fn give_up_on_current(&mut self) {
        if let Stage::Offering { order, reply, .. } = std::mem::take(&mut self.handover.stage) {
            self.offer_next(order, reply);
        }
    }

    /// The successor's answer. Anyone else's, or a stale one, is dropped.
    pub(super) fn ready(&mut self, device: PublicId, request: u64, contact: Option<Vec<u8>>) {
        let Stage::Offering {
            current,
            request: asked,
            ..
        } = &self.handover.stage
        else {
            return;
        };
        if *current != device || *asked != request {
            return;
        }
        let next_epoch = self.group.epoch().saturating_add(1);
        let checked = contact.filter(|sealed| {
            Contact::open(sealed, &device, self.lobby.room_id(), next_epoch).is_ok()
        });
        let Some(contact) = checked else {
            self.give_up_on_current();
            return;
        };
        let commit = match self.group.hand_over(&device) {
            Ok(commit) => commit,
            Err(e) => {
                self.emit(HostEvent::Error(e));
                self.give_up_on_current();
                return;
            }
        };
        let Stage::Offering { reply, .. } = std::mem::take(&mut self.handover.stage) else {
            return;
        };
        let others = self.others();
        self.broadcast_commit(&commit, &others);
        let removed: Vec<[u8; 32]> = self.handover.removed.iter().map(|d| d.0).collect();
        for member in &others {
            self.send_to(
                member,
                Control::HandedOver {
                    epoch: self.group.epoch(),
                    contact: contact.clone(),
                    removed: removed.clone(),
                },
            );
        }
        // Joiners waiting in this lobby cannot be admitted any more.
        let members: HashSet<PublicId> =
            self.group.members().into_iter().map(|(_, d)| d).collect();
        for joiner in self.lobby.seated() {
            if !members.contains(&joiner) {
                self.refuse(&joiner, RefuseReason::HostChanged);
            }
        }
        // Members now resume with the new host. Only the successor's link stays, to carry
        // the commit that removes this device.
        self.links.retain(|d, _| *d == device);
        self.recovering.clear();
        self.checking = None;
        self.latest_plan = None;
        self.handover.stage = Stage::Handed {
            successor: device,
            until: Instant::now() + LINGER_AFTER,
        };
        let _ = reply.send(Ok(device));
    }

    /// A commit sent back by the new host: the one that removes this device.
    pub(super) fn successor_commit(&mut self, device: PublicId, commit: &[u8]) {
        let Stage::Handed { successor, .. } = self.handover.stage else {
            return;
        };
        if device != successor {
            return;
        }
        if let Ok(Applied::Removed) = self.group.apply(commit) {
            self.handover.stage = Stage::Done(successor);
        }
    }

    /// Timeouts: a successor that did not answer, and the linger after the commit.
    pub(super) fn handover_tick(&mut self) {
        let now = Instant::now();
        match &self.handover.stage {
            Stage::Offering { until, current, .. }
                if *until <= now || !self.links.contains_key(current) =>
            {
                self.give_up_on_current();
            }
            Stage::Handed { successor, until } if *until <= now => {
                self.handover.stage = Stage::Done(*successor);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "handover_tests.rs"]
mod tests;
