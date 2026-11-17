//! The joiner's session: knock with an invite, join the group, follow the host's commits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use veil_group::{Applied, GroupError, Joiner};
use veil_net::control::Control;
use veil_net::{FramedRecv, Link, Node};
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
    /// The link to the host closed. The call is over for this member.
    Closed,
    /// A commit could not be applied, so this member's state no longer matches the group.
    /// The session stops; rejoin with a new invite.
    Failed(GroupError),
}

/// A joined member's session. Dropping it leaves the call.
pub struct MemberHandle {
    pub events: mpsc::UnboundedReceiver<MemberEvent>,
    link: Arc<Link>,
    path: Path,
    /// The virtual node of a relayed link to the host.
    _via: Option<Arc<Node>>,
    task: JoinHandle<()>,
}

impl Drop for MemberHandle {
    fn drop(&mut self) {
        self.task.abort();
        self.link.close("left");
    }
}

impl MemberHandle {
    /// The control link to the host.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// How the control link reached the host.
    pub fn path(&self) -> Path {
        self.path
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
    let addrs: Vec<SocketAddr> = invite.addrs.iter().filter_map(|a| a.parse().ok()).collect();
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
    out.send(&Control::KeyPackage(joiner.key_package()?)).await?;
    let welcome = match recv(&mut input, STEP).await? {
        Control::Welcome(welcome) => welcome,
        Control::Refused(reason) => return Err(CallError::Refused(reason)),
        _ => return Err(CallError::Protocol("welcome")),
    };
    let mut group = joiner.join(&welcome, &invite.host)?;
    let (host, room_id, me) = (invite.host, invite.room_id, call.identity().public());

    let (events, events_rx) = mpsc::unbounded_channel();
    let _ = events.send(MemberEvent::Keys(EpochKeys::from_group(&group)?));
    let task = tokio::spawn(async move {
        let ending = loop {
            let Ok(message) = input.recv::<Control>().await else {
                break MemberEvent::Closed;
            };
            match message {
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
                    let _ = events.send(event);
                }
                Control::Ping(n) if out.send(&Control::Pong(n)).await.is_err() => {
                    break MemberEvent::Closed;
                }
                _ => {}
            }
        };
        let _ = events.send(ending);
    });
    Ok(MemberHandle {
        events: events_rx,
        link,
        path,
        _via: via,
        task,
    })
}
