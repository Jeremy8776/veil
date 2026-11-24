//! Choosing this epoch's routes: mesh, or a forwarder every member has checked.
//!
//! Invariants:
//! - A forwarder is activated only when every current member succeeds for the current request.
//!   Refusal (including privacy policy) or timeout keeps the mesh. A replan drops the proposal.
//! - Any failure keeps mesh immediately; otherwise wait for all successes or the deadline.
//! - Each membership/address change returns to mesh and checks the complete new roster.
//! - Mesh routes are published before any forwarder is proposed, so a failed check never
//!   leaves the call without routes.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use veil_call::{CallError, CallNode, Directory, HostHandle, Routes};
use veil_net::NodeAddr;
use veil_security::PublicId;

use super::super::plan::plan_for_with_addresses;
use super::super::session::Session;

/// A probe batch is tied to one immutable roster and request. Replanning drops it, so an
/// answer from an old membership epoch or old address set cannot activate a replacement.
pub(super) struct PlanChecks {
    request: u64,
    required: HashSet<PublicId>,
    ready: HashSet<PublicId>,
    failed: HashSet<PublicId>,
    deadline: Instant,
}

/// What to do with a proposed forwarder.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Not every member has answered and the deadline has not passed.
    Waiting,
    /// Every current member checked the proposed route.
    Activate,
    /// Keep the mesh.
    Keep,
}

impl PlanChecks {
    fn new(
        request: u64,
        members: impl IntoIterator<Item = PublicId>,
        now: Instant,
    ) -> Self {
        Self {
            request,
            required: members.into_iter().collect(),
            ready: HashSet::new(),
            failed: HashSet::new(),
            deadline: now + Duration::from_secs(12),
        }
    }

    /// A member's first answer to the current request counts; later ones are ignored.
    pub(super) fn report(&mut self, device: PublicId, request: u64, ok: bool) {
        if request != self.request
            || Instant::now() >= self.deadline
            || !self.required.contains(&device)
            || self.ready.contains(&device)
            || self.failed.contains(&device)
        {
            return;
        }
        if ok {
            self.ready.insert(device);
        } else {
            self.failed.insert(device);
        }
    }

    fn verdict(&self, now: Instant) -> Verdict {
        if !self.failed.is_empty() {
            return Verdict::Keep;
        }
        if self.ready.len() == self.required.len() && !self.required.is_empty() {
            Verdict::Activate
        } else if now >= self.deadline {
            Verdict::Keep
        } else {
            Verdict::Waiting
        }
    }
}

pub(super) struct PendingPlan {
    pub(super) checks: PlanChecks,
    plan: veil_topology::Plan,
    directory: Directory,
    pub(super) own: Option<tokio::task::JoinHandle<Result<(), CallError>>>,
}

impl Drop for PendingPlan {
    fn drop(&mut self) {
        if let Some(task) = self.own.take() {
            task.abort();
        }
    }
}

/// Publishes mesh routes for the current epoch before proposing forwarding. Recheck the
/// entire roster on every replan, including newcomers to an already forwarded call.
/// A refusal or missing answer preserves mesh; no admitted participant is deliberately
/// left without media and relay-only members never have to make a direct forwarder probe.
#[allow(clippy::too_many_arguments)]
pub(super) async fn checked_plan(
    host: &HostHandle,
    call: &CallNode,
    session: &mut Session,
    roster: &[(u32, PublicId)],
    me: PublicId,
    addrs: &[SocketAddr],
    peers: &HashMap<PublicId, Vec<SocketAddr>>,
    forwarder: Option<NodeAddr>,
    next_request: &mut u64,
    forwarded: &mut bool,
) -> Option<PendingPlan> {
    if roster.len() <= 1 {
        *forwarded = false;
        return None;
    }
    let large = roster.len() > veil_topology::Policy::default().mesh_max;
    let mesh = plan_for_with_addresses(roster, me, addrs, None, peers);
    match mesh {
        Ok((plan, directory)) => match host.publish_plan(plan, directory).await {
            Ok(routes) => {
                *forwarded = false;
                session.on_routes(routes).await;
            }
            Err(e) => {
                crate::note!("cannot publish mesh routes: {e}");
                return None;
            }
        },
        Err(e) => {
            crate::note!("{e}");
            return None;
        }
    }
    let forwarder = forwarder?;
    if !large {
        return None;
    }
    let (plan, directory) = match plan_for_with_addresses(roster, me, addrs, Some(forwarder), peers)
    {
        Ok(candidate) => candidate,
        Err(e) => {
            crate::note!("cannot propose forwarder: {e}");
            return None;
        }
    };
    *next_request = next_request.wrapping_add(1).max(1);
    let request = *next_request;
    let route = match host
        .probe_plan(plan.clone(), directory.clone(), request)
        .await
    {
        Ok(route) => route,
        Err(e) => {
            crate::note!("cannot check proposed forwarder: {e}");
            return None;
        }
    };
    let Routes::Forwarder { addr, ticket } = route else {
        return None;
    };
    let own = tokio::spawn(call.probe_forwarder(addr, ticket));
    Some(PendingPlan {
        checks: PlanChecks::new(
            request,
            roster.iter().map(|(_, device)| *device),
            Instant::now(),
        ),
        plan,
        directory,
        own: Some(own),
    })
}

pub(super) async fn own_check(pending: &mut Option<PendingPlan>) -> (u64, bool) {
    if let Some(pending) = pending
        && let Some(task) = pending.own.as_mut()
    {
        return (
            pending.checks.request,
            task.await.is_ok_and(|result| result.is_ok()),
        );
    }
    std::future::pending().await
}

pub(super) async fn finish_checked_plan(
    pending: &mut Option<PendingPlan>,
    host: &HostHandle,
    session: &mut Session,
    forwarded: &mut bool,
) {
    let Some(candidate) = pending.as_ref() else {
        return;
    };
    match candidate.checks.verdict(Instant::now()) {
        Verdict::Waiting => return,
        Verdict::Keep => {
            crate::note!("forwarder not checked by every member; keeping mesh routes");
            pending.take();
            return;
        }
        Verdict::Activate => {},
    }
    let Some(candidate) = pending.take() else {
        return;
    };
    match host
        .publish_plan(candidate.plan.clone(), candidate.directory.clone())
        .await
    {
        Ok(routes) => {
            crate::note!("every member checked the forwarder; changing media routes");
            *forwarded = true;
            session.on_routes(routes).await;
        }
        Err(e) => crate::note!("cannot activate checked forwarder; keeping mesh routes: {e}"),
    }
}

#[cfg(test)]
mod integration;

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicId {
        PublicId([byte; 32])
    }

    fn five(request: u64, now: Instant) -> PlanChecks {
        PlanChecks::new(request, (1..=5).map(key), now)
    }

    #[test]
    fn duplicates_old_answers_and_strangers_count_for_nothing() {
        let now = Instant::now();
        let mut checks = five(12, now);
        checks.report(key(1), 12, true);
        checks.report(key(1), 12, true);
        checks.report(key(2), 11, true);
        checks.report(key(99), 12, true);
        assert_eq!(checks.verdict(now), Verdict::Waiting);
        assert_eq!(
            checks.verdict(now + Duration::from_secs(12)),
            Verdict::Keep,
            "every member must confirm"
        );
    }

    #[test]
    fn every_member_checking_activates_at_once() {
        let now = Instant::now();
        let mut checks = five(3, now);
        (1..=5).for_each(|k| checks.report(key(k), 3, true));
        assert_eq!(checks.verdict(now), Verdict::Activate);
    }

    #[test]
    fn a_single_refusal_keeps_the_mesh_and_cannot_be_overwritten() {
        let now = Instant::now();
        let mut checks = five(4, now);
        (1..=4).for_each(|k| checks.report(key(k), 4, true));
        checks.report(key(5), 4, false);
        checks.report(key(5), 4, true);
        assert_eq!(checks.verdict(now), Verdict::Keep);
    }

    #[test]
    fn a_silent_member_keeps_the_mesh_after_the_deadline() {
        let now = Instant::now();
        let mut checks = five(5, now);
        (1..=4).for_each(|k| checks.report(key(k), 5, true));
        assert_eq!(checks.verdict(now + Duration::from_secs(11)), Verdict::Waiting);
        assert_eq!(
            checks.verdict(now + Duration::from_secs(12)),
            Verdict::Keep
        );
    }

    #[test]
    fn the_host_failing_or_most_members_failing_keeps_the_mesh() {
        let now = Instant::now();
        let mut checks = five(6, now);
        checks.report(key(1), 6, false);
        assert_eq!(checks.verdict(now), Verdict::Keep);

        let mut checks = five(7, now);
        (1..=2).for_each(|k| checks.report(key(k), 7, true));
        (3..=5).for_each(|k| checks.report(key(k), 7, false));
        assert_eq!(checks.verdict(now), Verdict::Keep);
    }

    #[test]
    fn expired_answers_and_a_replaced_roster_cannot_activate() {
        let now = Instant::now();
        let mut expired = five(8, now - Duration::from_secs(13));
        (1..=5).for_each(|k| expired.report(key(k), 8, true));
        assert!(expired.ready.is_empty());
        assert_eq!(expired.verdict(now), Verdict::Keep);

        let mut replacement = PlanChecks::new(9, [key(1), key(2), key(3), key(4), key(6)], now);
        (1..=5).for_each(|k| replacement.report(key(k), 8, true));
        assert!(replacement.ready.is_empty(), "answers from the removed roster do not carry over");
        (1..=5).for_each(|k| replacement.report(key(k), 9, true));
        assert_eq!(replacement.verdict(now), Verdict::Waiting, "the newcomer has not checked");
        replacement.report(key(6), 9, true);
        assert_eq!(replacement.verdict(now), Verdict::Activate);
    }
}
