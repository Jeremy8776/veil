//! Choosing this epoch's routes: mesh, or a forwarder every member has checked.
//!
//! Invariants:
//! - A forwarder is activated only when the host's own check and a strict majority of current
//!   members succeed for the current request. One member failing, or never answering, cannot
//!   hold a large call on the mesh; it only waits for the deadline. A replan drops the proposal.
//! - The decision waits until every member answered or the deadline passed, whichever is first.
//! - A call already on the forwarder stays on it; its new roster ticket drops removed members.
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
    /// The host. Its own check must succeed: it chose the forwarder and carries the call.
    host: PublicId,
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
    /// Switch to the forwarder. `left_out` members failed or never answered.
    Activate { left_out: usize },
    /// Keep the mesh.
    Keep,
}

impl PlanChecks {
    fn new(
        request: u64,
        host: PublicId,
        members: impl IntoIterator<Item = PublicId>,
        now: Instant,
    ) -> Self {
        Self {
            request,
            host,
            required: members.into_iter().collect(),
            ready: HashSet::new(),
            failed: HashSet::new(),
            deadline: now + Duration::from_secs(12),
        }
    }

    /// A member's first answer to the current request counts; later ones are ignored.
    pub(super) fn report(&mut self, device: PublicId, request: u64, ok: bool) {
        if request != self.request
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
        let answered = self.ready.len() + self.failed.len();
        if self.failed.contains(&self.host) {
            return Verdict::Keep;
        }
        if answered < self.required.len() && now < self.deadline {
            return Verdict::Waiting;
        }
        // shortcut: members left out get no media until a plan can mix mesh and forwarder
        // routes. Upgrade when `veil-topology` plans per-member routes.
        if self.ready.contains(&self.host) && self.ready.len() * 2 > self.required.len() {
            Verdict::Activate {
                left_out: self.required.len() - self.ready.len(),
            }
        } else {
            Verdict::Keep
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

/// Publishes this epoch's routes. A call already on the forwarder stays there: its plan is
/// republished with the new roster ticket, which is also how the forwarder drops a removed
/// member. Otherwise mesh routes go out first and a forwarder is only proposed, never
/// activated, until every member has checked it. The probes run on their own tasks.
///
/// shortcut: a member who joins a forwarded call is not probed first; if it cannot reach the
/// forwarder it gets no media. Probe newcomers and fall back to mesh when calls cross networks.
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
    if *forwarded
        && large
        && let Some(forwarder) = forwarder
    {
        match plan_for_with_addresses(roster, me, addrs, Some(forwarder), peers) {
            Ok((plan, directory)) => match host.publish_plan(plan, directory).await {
                Ok(routes) => {
                    session.on_routes(routes).await;
                    return None;
                }
                Err(e) => crate::note!("cannot keep the forwarder; trying mesh routes: {e}"),
            },
            Err(e) => crate::note!("cannot keep the forwarder; trying mesh routes: {e}"),
        }
    }
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
            me,
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
    let left_out = match candidate.checks.verdict(Instant::now()) {
        Verdict::Waiting => return,
        Verdict::Keep => {
            crate::note!("forwarder not reachable by most members; keeping mesh routes");
            pending.take();
            return;
        }
        Verdict::Activate { left_out } => left_out,
    };
    let Some(candidate) = pending.take() else {
        return;
    };
    match host
        .publish_plan(candidate.plan.clone(), candidate.directory.clone())
        .await
    {
        Ok(routes) => {
            if left_out > 0 {
                crate::note!(
                    "{left_out} member(s) could not check the forwarder; changing media routes anyway"
                );
            } else {
                crate::note!("every member checked the forwarder; changing media routes");
            }
            *forwarded = true;
            session.on_routes(routes).await;
        }
        Err(e) => crate::note!("cannot activate checked forwarder; keeping mesh routes: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicId {
        PublicId([byte; 32])
    }

    fn five(request: u64, now: Instant) -> PlanChecks {
        PlanChecks::new(request, key(1), (1..=5).map(key), now)
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
            "one real success out of five is not a majority"
        );
    }

    #[test]
    fn every_member_checking_activates_at_once() {
        let now = Instant::now();
        let mut checks = five(3, now);
        (1..=5).for_each(|k| checks.report(key(k), 3, true));
        assert_eq!(checks.verdict(now), Verdict::Activate { left_out: 0 });
    }

    #[test]
    fn one_member_failing_cannot_hold_the_call_on_the_mesh() {
        let now = Instant::now();
        let mut checks = five(4, now);
        (1..=4).for_each(|k| checks.report(key(k), 4, true));
        checks.report(key(5), 4, false);
        checks.report(key(5), 4, true);
        assert_eq!(checks.verdict(now), Verdict::Activate { left_out: 1 });
    }

    #[test]
    fn a_silent_member_only_delays_the_switch_to_the_deadline() {
        let now = Instant::now();
        let mut checks = five(5, now);
        (1..=4).for_each(|k| checks.report(key(k), 5, true));
        assert_eq!(checks.verdict(now + Duration::from_secs(11)), Verdict::Waiting);
        assert_eq!(
            checks.verdict(now + Duration::from_secs(12)),
            Verdict::Activate { left_out: 1 }
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
}
