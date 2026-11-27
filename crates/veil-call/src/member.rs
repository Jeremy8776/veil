//! The joiner's session: knock with an invite, join the group, follow the host's commits.
//!
//! In a persistent call the host may hand over (ADR 0010). Only messages from the device the
//! group names as host count: once the handover commit is merged, `Ended`, plans and
//! address hints from the old host are ignored. See `member/handover.rs`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use veil_group::{Applied, GroupError, Joiner};
use veil_net::control::{Availability, Control};
use veil_net::{FramedRecv, FramedSend, Link, Node};
use veil_security::PublicId;
use veil_security::invite::Invite;

mod handover;
mod recover;

pub use handover::Promotion;
use handover::{MemberCommand, Next, decide};
use recover::{recover_control, wait_to_follow};

use crate::events::EventSender;
use crate::node::{CallNode, Path};
use crate::relay_field::RelayAddrs;
use crate::routes::Routes;
use crate::{CallError, CallPlan, EpochKeys};

/// How long a joiner waits in the lobby for the host to decide.
pub const LOBBY_WAIT: Duration = Duration::from_secs(600);
/// How long each later step of the handshake may take.
const STEP: Duration = Duration::from_secs(10);
/// How long a new host waits for the old host to take its removal commit and hang up. Kept
/// short: members are already dialling the new host.
const OLD_HOST_CLOSE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum MemberEvent {
    /// The application fell behind the bounded event queue. This session has stopped.
    /// Rejoin with a fresh invitation; no keys or membership changes were silently skipped.
    Overloaded,
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
    /// Persistent call: the host asks this device to take over. Start host roles, then
    /// answer with [`MemberHandle::handover_ready`] within 10 seconds.
    HandoverOffer { request: u64, epoch: u64 },
    /// This device is now the host. The session has stopped; pass this to
    /// [`crate::HostHandle::take_over`] and [`crate::CallNode::promote`].
    Promoted(Box<Promotion>),
    /// The host handed over to `host`. Move the node with [`crate::CallNode::follow_host`],
    /// then call [`MemberHandle::follow`]. When `ask` is set this relay-only member's relay
    /// changes operator: ask the person first, and drop the session if they decline.
    HostChanging {
        host: PublicId,
        relay: Option<RelayAddrs>,
        ask: bool,
    },
    /// The new host accepted this member. Show "Ana is now the host."
    HostChanged(PublicId),
    /// The call could not continue while the host changed. A fresh invitation is needed.
    LostInHandover,
}

/// A joined member's session. Dropping it leaves the call.
pub struct MemberHandle {
    /// Ordered, bounded events; dropping the receiver stops control processing.
    pub events: mpsc::Receiver<MemberEvent>,
    commands: mpsc::Sender<MemberCommand>,
    persistent: bool,
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

    /// The call outlives its host (ADR 0010): show Available to host.
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    async fn command(&self, command: MemberCommand) -> Result<(), CallError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| CallError::Closed)
    }

    /// Answers [`MemberEvent::HandoverOffer`] with this device's sealed
    /// [`crate::host::contact::Contact`] for the next epoch, or `None` to decline.
    pub async fn handover_ready(
        &self,
        request: u64,
        contact: Option<Vec<u8>>,
    ) -> Result<(), CallError> {
        self.command(MemberCommand::Ready { request, contact }).await
    }

    /// After [`MemberEvent::HostChanging`] and [`crate::CallNode::follow_host`]: dial the new
    /// host. Not calling it within the recovery window loses the call.
    pub async fn follow(&self, call: &CallNode) -> Result<(), CallError> {
        let (node, presence, relay_only) = call.redial_context();
        self.command(MemberCommand::Follow {
            node,
            presence,
            relay_only,
        })
        .await
    }

    /// Persistent call: tells the host whether this device can take over, with coarse
    /// figures only. A relay-only device always reports that it is not available.
    pub async fn report_availability(&self, report: Availability) -> Result<(), CallError> {
        self.command(MemberCommand::Availability(report)).await
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
    let (mut host, room_id, me) = (invite.host, invite.room_id, call.identity().public());
    let (mut node, mut presence, mut relay_only) = call.redial_context();
    let mut relay_key = invite.relay.as_deref().and_then(RelayAddrs::parse).map(|r| r.key);
    let (commands_tx, mut commands) = mpsc::channel(4);
    let persistent = group.is_persistent();
    let mut candidates = own_hints(&node, relay_only);
    out.send(&Control::Addresses(candidates.clone())).await?;

    let (events, events_rx) = EventSender::channel();
    events.emit(MemberEvent::Keys(EpochKeys::from_group(&group)?));
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
        // Set while following a handover: the next reconnect goes to the new host.
        let mut changing: Option<PublicId> = None;
        let ending = loop {
            let message = tokio::select! {
                () = events.closed() => break MemberEvent::Closed,
                message = inbox.recv() => match message {
                    Some(message) => message,
                    None => {
                        if changing.is_none() && live.peer_closed_with("bye") { break MemberEvent::Closed; }
                        if changing.is_none() && !events.emit(MemberEvent::Reconnecting) { break MemberEvent::Overloaded; }
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
                                        Ok(keys) => { if !events.emit(MemberEvent::Keys(keys)) { break MemberEvent::Overloaded; } }
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
                                let done = match changing.take() {
                                    Some(new) => MemberEvent::HostChanged(new),
                                    None => MemberEvent::Reconnected,
                                };
                                if !events.emit(done) { break MemberEvent::Overloaded; }
                                continue;
                            }
                            Err(CallError::Group(e)) => break MemberEvent::Failed(e),
                            Err(CallError::Refused(_)) => break MemberEvent::Removed,
                            Err(_) if changing.is_some() => break MemberEvent::LostInHandover,
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
                Some(command) = commands.recv() => {
                    let message = match command {
                        MemberCommand::Ready { request, contact } => {
                            Some(Control::HandoverReady { request, contact })
                        }
                        MemberCommand::Availability(mut report) => {
                            report.available &= !relay_only;
                            Some(Control::Availability(report))
                        }
                        // Only meaningful while waiting in `wait_to_follow`.
                        MemberCommand::Follow { .. } => None,
                    };
                    if let Some(message) = message
                        && send_control(&mut out, &message).await.is_err()
                    {
                        live.close("control write failed");
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
            // After a handover commit the link's device is an ordinary member: it can no
            // longer end the call, plan it or point this member anywhere.
            let authority = group.host() == Some(host);
            match message {
                Control::Ended if authority => break MemberEvent::Closed,
                Control::HandoverOffer { request, epoch } if authority && epoch == group.epoch() => {
                    if !events.emit(MemberEvent::HandoverOffer { request, epoch }) { break MemberEvent::Overloaded; }
                }
                handed @ Control::HandedOver { .. } if handed.within_bounds() => {
                    let Control::HandedOver { epoch, contact, removed } = handed else { continue };
                    match decide(&group, host, room_id, me, epoch, &contact, relay_only) {
                        Next::Ignore => {}
                        Next::Lost => break MemberEvent::LostInHandover,
                        Next::TakeOver => {
                            let removal = match group.remove(&host) {
                                Ok(removal) => removal,
                                Err(e) => break MemberEvent::Failed(e),
                            };
                            // The old host waits for this commit, then closes its side. Closing
                            // ours at once could drop the commit unsent, so give it a moment.
                            if send_control(&mut out, &Control::Commit(removal.clone())).await.is_ok() {
                                let until = tokio::time::Instant::now() + OLD_HOST_CLOSE;
                                while let Ok(Some(_)) = tokio::time::timeout_at(until, inbox.recv()).await {}
                            }
                            break MemberEvent::Promoted(Box::new(Promotion {
                                journal: vec![(group.epoch(), removal)],
                                removed: removed.into_iter().map(PublicId).collect(),
                                old_host: host,
                                room_id,
                                group,
                            }));
                        }
                        Next::Follow(new, contact) => {
                            let relay = contact.relay_addrs();
                            let ask = relay_only && relay.as_ref().map(|r| r.key) != relay_key;
                            if !events.emit(MemberEvent::HostChanging { host: new, relay: relay.clone(), ask }) {
                                break MemberEvent::Overloaded;
                            }
                            let Some(next) = wait_to_follow(&mut commands).await else {
                                break MemberEvent::LostInHandover;
                            };
                            (node, presence, relay_only) = next;
                            host = new;
                            relay_key = relay.map(|r| r.key);
                            addrs = if relay_only { Vec::new() } else { contact.addrs };
                            changing = Some(new);
                            // Close the old host's link. The empty inbox makes the loop's
                            // reconnect path dial the new host with `Resume`.
                            drop(probe.take());
                            live.close("host changed");
                            let (_, closed) = mpsc::channel(1);
                            inbox = closed;
                            reader = Reader(tokio::spawn(async {}));
                        }
                    }
                }
                Control::ProbePlan { .. } | Control::Plan(_) | Control::Addresses(_) if !authority => {}
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
                            if !events.emit(MemberEvent::Keys(keys)) { break MemberEvent::Overloaded; }
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
                    if !events.emit(event) { break MemberEvent::Overloaded; }
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
        live.close("left");
        events.finish(ending);
    });
    Ok(MemberHandle {
        events: events_rx,
        commands: commands_tx,
        persistent,
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
