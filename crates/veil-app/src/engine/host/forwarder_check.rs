//! Choosing this epoch's routes: mesh, or a forwarder every member has checked.
//!
//! Invariants:
//! - A forwarder is only activated after every current member answered the current request
//!   with success before the deadline. A failure, a late answer or a replan drops the proposal.
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
    required: HashSet<PublicId>,
    ready: HashSet<PublicId>,
    failed: bool,
    deadline: Instant,
}

impl PlanChecks {
    fn new(request: u64, members: impl IntoIterator<Item = PublicId>, now: Instant) -> Self {
        Self {
            request,
            required: members.into_iter().collect(),
            ready: HashSet::new(),
            failed: false,
            deadline: now + Duration::from_secs(12),
        }
    }

    pub(super) fn report(&mut self, device: PublicId, request: u64, ok: bool) {
        if request != self.request || !self.required.contains(&device) {
            return;
        }
        if ok {
            self.ready.insert(device);
        } else {
            self.failed = true;
        }
    }

    fn ready(&self) -> bool {
        !self.failed && !self.required.is_empty() && self.ready == self.required
    }

    fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
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
    // Expiry wins even if the final answer arrived late in this tick.
    if candidate.checks.failed || candidate.checks.expired(Instant::now()) {
        crate::note!("forwarder not reachable by every member; keeping mesh routes");
        pending.take();
        return;
    }
    if !candidate.checks.ready() {
        return;
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
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicId {
        PublicId([byte; 32])
    }

    #[test]
    fn a_forwarder_needs_every_members_own_current_answer() {
        let mut checks = PlanChecks::new(12, [key(1), key(2), key(3)], Instant::now());
        checks.report(key(1), 12, true);
        checks.report(key(1), 12, true);
        checks.report(key(2), 11, true);
        checks.report(key(99), 12, true);
        assert!(
            !checks.ready(),
            "duplicates, old answers and strangers cannot fill the roster"
        );
        checks.report(key(2), 12, true);
        assert!(!checks.ready(), "the final participant must check too");
        checks.report(key(3), 12, true);
        assert!(checks.ready());
    }

    #[test]
    fn a_failure_cannot_be_overwritten_by_a_later_success() {
        let mut checks = PlanChecks::new(7, [key(1), key(2)], Instant::now());
        checks.report(key(1), 7, true);
        checks.report(key(2), 7, false);
        checks.report(key(2), 7, true);
        assert!(!checks.ready());
        assert!(checks.failed);
    }

    #[test]
    fn replanning_requires_fresh_answers_and_has_a_deadline() {
        let now = Instant::now();
        let mut checks = PlanChecks::new(8, [key(1), key(2)], now);
        checks.report(key(1), 7, true);
        checks.report(key(2), 7, true);
        assert!(
            !checks.ready(),
            "the previous roster's batch must not activate this one"
        );
        assert!(!checks.expired(now + Duration::from_secs(11)));
        assert!(checks.expired(now + Duration::from_secs(12)));
    }
}
