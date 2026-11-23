//! The joiner's session: knock with an invite, join the group, follow the host's commits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use veil_group::{Applied, Group, GroupError, Joiner};
use veil_net::control::Control;
use veil_net::{FramedRecv, FramedSend, Link, Node};
use veil_security::invite::Invite;

use crate::node::{CallNode, Path};
use crate::routes::Routes;
use crate::{CallError, CallPlan, EpochKeys};

/// How long a joiner waits in the lobby for the host to decide.
pub const LOBBY_WAIT: Duration = Duration::from_secs(600);
/// How long each later step of the handshake may take.
const STEP: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum MemberEvent {
    /// A new epoch started. Replace the media pipeline's frame keys.
    Keys(EpochKeys),
    /// The host's layout for the current epoch. Apply it to the media session.
    Routes(Routes),
    /// The host sent a plan this member cannot follow. The previous routes still stand.
    PlanRefused(CallError),
    /// The host removed this member. No later keys will arrive.
    Removed,
    /// An unexpected transport loss is being recovered without changing membership.
    Reconnecting,
    /// The pinned host accepted recovery and all missed commits have been applied.
    Reconnected,
    /// The host intentionally ended the call.
    Closed,
    /// The bounded recovery attempt failed; a fresh invitation is needed.
    ConnectionLost,
    /// A commit could not be applied, so this member's state no longer matches the group.
    /// The session stops; rejoin with a new invite.
    Failed(GroupError),
}

/// A joined member's session. Dropping it leaves the call.
pub struct MemberHandle {
    pub events: mpsc::UnboundedReceiver<MemberEvent>,
    current: watch::Receiver<(Arc<Link>, Path)>,
    task: JoinHandle<()>,
}

impl Drop for MemberHandle {
    fn drop(&mut self) {
        self.task.abort();
        self.current.borrow().0.close("left");
    }
}

impl MemberHandle {
    /// The control link to the host.
    pub fn link(&self) -> Arc<Link> {
        self.current.borrow().0.clone()
    }

    /// How the control link reached the host.
    pub fn path(&self) -> Path {
        self.current.borrow().1
    }
}

async fn recv(input: &mut FramedRecv, wait: Duration) -> Result<Control, CallError> {
    tokio::time::timeout(wait, input.recv::<Control>())
        .await
        .map_err(|_| CallError::Timeout)?
        .map_err(CallError::from)
}

/// Joins the call an invite points to. Returns once the host has admitted this device and its
/// Welcome has been applied; the first event is the joined epoch's keys.
///
/// The host is dialled at the invite's direct addresses first. When that fails and `call` has
/// a rendezvous node (the invite's relay), a punched dial and then a relay session are tried.
/// A relay-only node skips straight to the relay.
pub async fn join(call: &CallNode, invite: &Invite) -> Result<MemberHandle, CallError> {
    join_with(call, invite, || {}).await
}

/// [`join`], calling `knocked` once the host has been reached and asked to admit this device.
/// Before that, the time goes on finding a path to the host (direct, punched, relayed); after
/// it, on waiting for the host's decision. An interface shows the two differently.
pub async fn join_with(
    call: &CallNode,
    invite: &Invite,
    knocked: impl FnOnce(),
) -> Result<MemberHandle, CallError> {
    if invite.room_id != call.room_id() {
        return Err(CallError::Protocol("a node started for this invite's room"));
    }
    let mut addrs: Vec<SocketAddr> = invite.addrs.iter().filter_map(|a| a.parse().ok()).collect();
    let dialled = call.dial(invite.host, &addrs).await?;
    let (path, via) = (dialled.path, dialled.via);
    let link = Arc::new(dialled.link);
    let (mut out, mut input) = link.open_control().await?.split();

    out.send(&Control::Hello {
        room_id: invite.room_id,
        join_token: invite.join_token,
    })
    .await?;
    knocked();
    match recv(&mut input, LOBBY_WAIT).await? {
        Control::Admitted => {}
        Control::Refused(reason) => return Err(CallError::Refused(reason)),
        _ => return Err(CallError::Protocol("admitted or refused")),
    }

    let joiner = Joiner::new(call.identity())?;
    out.send(&Control::KeyPackage(joiner.key_package()?))
        .await?;
    let welcome = match recv(&mut input, STEP).await? {
        Control::Welcome(welcome) => welcome,
        Control::Refused(reason) => return Err(CallError::Refused(reason)),
        _ => return Err(CallError::Protocol("welcome")),
    };
    let mut group = joiner.join(&welcome, &invite.host)?;
    let (host, room_id, me) = (invite.host, invite.room_id, call.identity().public());
    let (node, presence, relay_only) = call.redial_context();
    let mut candidates = own_hints(&node, relay_only);
    out.send(&Control::Addresses(candidates.clone())).await?;

    let (events, events_rx) = mpsc::unbounded_channel();
    let _ = events.send(MemberEvent::Keys(EpochKeys::from_group(&group)?));
    let (current, current_rx) = watch::channel((link.clone(), path));
    let task = tokio::spawn(async move {
        let mut live = link;
        let mut _via = via;
        // One dedicated reader owns each framed stream; select never cancels a partial read.
        let (mut reader, mut inbox) = start_reader(input);
        let mut refresh = tokio::time::interval(Duration::from_secs(2));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (checked, mut checks) = mpsc::channel(1);
        let mut probe: Option<Reader> = None;
        let ending = loop {
            let message = tokio::select! {
                message = inbox.recv() => match message {
                    Some(message) => message,
                    None => {
                        if live.peer_closed_with("bye") { break MemberEvent::Closed; }
                        let _ = events.send(MemberEvent::Reconnecting);
                        drop(probe.take());
                        while checks.try_recv().is_ok() {}
                        let before = group.epoch();
                        match recover_control(&node, presence.as_ref(), relay_only, host, &addrs, room_id, &mut group).await {
                            Ok((dialled, send, input)) => {
                                live = Arc::new(dialled.link);
                                _via = dialled.via;
                                out = send;
                                let next = start_reader(input);
                                reader = next.0;
                                inbox = next.1;
                                let _ = current.send((live.clone(), dialled.path));
                                if group.epoch() != before {
                                    match EpochKeys::from_group(&group) {
                                        Ok(keys) => { let _ = events.send(MemberEvent::Keys(keys)); }
                                        Err(e) => break MemberEvent::Failed(e),
                                    }
                                }
                                // The new control connection receives its own address update,
                                // even if the local address has not changed since the outage.
                                candidates = own_hints(&node, relay_only);
                                if send_control(&mut out, &Control::Addresses(candidates.clone())).await.is_err() {
                                    live.close("control recovery failed");
                                    continue;
                                }
                                let _ = events.send(MemberEvent::Reconnected);
                                continue;
                            }
                            Err(CallError::Group(e)) => break MemberEvent::Failed(e),
                            Err(CallError::Refused(_)) => break MemberEvent::Removed,
                            Err(_) => break MemberEvent::ConnectionLost,
                        }
                    },
                },
                _ = refresh.tick() => {
                    let next = own_hints(&node, relay_only);
                    if next != candidates {
                        candidates = next;
                        if send_control(&mut out, &Control::Addresses(candidates.clone())).await.is_err() {
                            live.close("control write failed");
                        }
                    }
                    continue;
                },
                Some((request, epoch, ok)) = checks.recv() => {
                    if epoch == group.epoch() && send_control(&mut out, &Control::PlanChecked { request, ok }).await.is_err() {
                        live.close("control write failed");
                    }
                    continue;
                },
            };
            match message {
                Control::Ended => break MemberEvent::Closed,
                Control::ProbePlan { request, sealed } => {
                    // A host-signed plan binds the target to this room and current epoch.
                    // Probe asynchronously so a dead forwarder cannot delay removal commits.
                    let routes = CallPlan::open(&sealed, &host, room_id, group.epoch())
                        .and_then(|plan| plan.routes_for(&me));
                    let (node, checked, epoch) = (node.clone(), checked.clone(), group.epoch());
                    probe = Some(Reader(tokio::spawn(async move {
                        let ok = match routes {
                            Ok(Routes::Forwarder { addr, ticket }) if !relay_only => {
                                matches!(
                                    tokio::time::timeout(
                                        Duration::from_secs(8),
                                        veil_forward::ForwarderClient::probe(
                                            &node,
                                            &[addr.addr],
                                            addr.key,
                                            &ticket
                                        )
                                    )
                                    .await,
                                    Ok(Ok(()))
                                )
                            }
                            Ok(Routes::Forwarder { .. }) => false,
                            Ok(Routes::Mesh { .. }) => true,
                            Err(_) => false,
                        };
                        let _ = checked.send((request, epoch, ok)).await;
                    })));
                }
                Control::Commit(commit) => match group.apply(&commit) {
                    Ok(Applied::NewEpoch(_)) => match EpochKeys::from_group(&group) {
                        Ok(keys) => {
                            let _ = events.send(MemberEvent::Keys(keys));
                        }
                        Err(e) => break MemberEvent::Failed(e),
                    },
                    Ok(Applied::Removed) => break MemberEvent::Removed,
                    Err(e) => break MemberEvent::Failed(e),
                },
                Control::Plan(sealed) => {
                    let event = CallPlan::open(&sealed, &host, room_id, group.epoch())
                        .and_then(|plan| plan.routes_for(&me))
                        .map_or_else(MemberEvent::PlanRefused, MemberEvent::Routes);
                    if !relay_only
                        && let MemberEvent::Routes(Routes::Mesh { peers, .. }) = &event
                        && let Some(peer) = peers.iter().find(|peer| peer.device == host)
                    {
                        addrs = peer.addrs.clone();
                    }
                    let _ = events.send(event);
                }
                Control::Addresses(next) => {
                    // This stream is pinned to the host. Never learn addresses from any
                    // other member, and keep relay-only recovery from exposing our address.
                    if !relay_only && let Some(next) = crate::host::checked_candidates(next) {
                        addrs = next;
                    }
                }
                Control::Ping(n) if send_control(&mut out, &Control::Pong(n)).await.is_err() => {
                    live.close("control write failed");
                }
                _ => {}
            }
        };
        drop(probe);
        drop(reader);
        let _ = events.send(ending);
    });
    Ok(MemberHandle {
        events: events_rx,
        current: current_rx,
        task,
    })
}

/// The addresses this member tells the host about, which reach every other member.
///
/// Only in a Local only call, and only local scope ones. In any other call the members may be
/// across the internet: a full list of private and link-local addresses would fingerprint
/// this machine (a link-local IPv6 address can carry the network card's MAC address), and
/// they already see the public address this link comes from. Hide my address sends none.
fn own_hints(node: &Node, relay_only: bool) -> Vec<SocketAddr> {
    if relay_only || !node.is_local_only() {
        return Vec::new();
    }
    node.local_addr()
        .map(|a| veil_net::local::local_candidates(a.port()))
        .unwrap_or_default()
}

/// A background reader or probe must not outlive its member session.
struct Reader(JoinHandle<()>);
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// New control streams always get their own reader. Dropping its guard stops it.
fn start_reader(mut input: FramedRecv) -> (Reader, mpsc::Receiver<Control>) {
    let (messages, inbox) = mpsc::channel(72);
    let reader = Reader(tokio::spawn(async move {
        while let Ok(message) = input.recv::<Control>().await {
            if messages.send(message).await.is_err() {
                break;
            }
        }
    }));
    (reader, inbox)
}

async fn send_control(out: &mut FramedSend, message: &Control) -> Result<(), CallError> {
    tokio::time::timeout(Duration::from_secs(3), out.send(message))
        .await
        .map_err(|_| CallError::Timeout)?
        .map_err(CallError::Net)
}

/// Recovery authenticates the same device and pinned host. It never reuses a lobby token or
/// creates a new membership. Only contiguous host-authenticated commits advance the group.
async fn recover_control(
    node: &Node,
    presence: Option<&crate::presence::Presence>,
    relay_only: bool,
    host: veil_security::PublicId,
    addrs: &[SocketAddr],
    room_id: [u8; 16],
    group: &mut Group,
) -> Result<(crate::dial::Dialled, FramedSend, FramedRecv), CallError> {
    let deadline = tokio::time::Instant::now() + crate::host::CONTROL_RECOVERY;
    let mut gap = Duration::from_millis(200);
    loop {
        let attempt = async {
            let dialled = crate::dial::dial(node, presence, host, addrs, relay_only).await?;
            let (mut out, mut input) = dialled.link.open_control().await?.split();
            send_control(
                &mut out,
                &Control::Resume {
                    room_id,
                    epoch: group.epoch(),
                },
            )
            .await?;
            let current = match recv(&mut input, STEP).await? {
                Control::Resumed { epoch }
                    if epoch >= group.epoch() && epoch - group.epoch() <= 64 =>
                {
                    epoch
                }
                Control::Refused(reason) => return Err(CallError::Refused(reason)),
                _ => return Err(CallError::Protocol("resume acknowledgment")),
            };
            while group.epoch() < current {
                let Control::Commit(commit) = recv(&mut input, STEP).await? else {
                    return Err(CallError::Protocol("recovery commit"));
                };
                match group.apply(&commit)? {
                    Applied::NewEpoch(_) => {}
                    Applied::Removed => {
                        return Err(CallError::Refused(
                            veil_net::control::RefuseReason::DeclinedByHost,
                        ));
                    }
                }
            }
            Ok((dialled, out, input))
        };
        match tokio::time::timeout_at(deadline, attempt).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(e @ (CallError::Refused(_) | CallError::Group(_) | CallError::Protocol(_)))) => {
                return Err(e);
            }
            Err(_) => return Err(CallError::Timeout),
            Ok(Err(_)) => {}
        }
        tokio::time::timeout_at(deadline, tokio::time::sleep(gap))
            .await
            .map_err(|_| CallError::Timeout)?;
        gap = (gap * 2).min(Duration::from_secs(3));
    }
}
