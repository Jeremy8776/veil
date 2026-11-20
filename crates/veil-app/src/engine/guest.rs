//! Joining a call from an invite: wait in the lobby, then follow the host's keys and plan.
//!
//! Purpose: starts a network node registered with the invite's rendezvous, calls
//! `veil_call::join` (which waits until the host decides), then runs the guest's loop: each
//! `MemberEvent::Keys` replaces the frame keys and each `MemberEvent::Routes` is applied to
//! the `MediaSession`.
//!
//! Invariants:
//! - Leaving before admission (reaching the host, or in the lobby) cancels the join at once;
//!   nothing was admitted, so nothing to undo.
//! - The status is Connecting while a path to the host is found, and Lobby only once the
//!   host has the knock, so "waiting for the host" is never shown while still dialling.
//! - Being removed, or the host ending the call, ends it here with a reason the end screen
//!   shows.
//! - Linking up can take seconds, but the end of the call and Leave are acted on at once
//!   (`link_up`). Everything else that arrives meanwhile is handled afterwards, in order.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallError, CallNode, MemberEvent, join_with};
use veil_net::Node;
use veil_net::control::RefuseReason;
use veil_rendezvous::fetch_invite;
use veil_security::Identity;
use veil_security::short_invite::InviteLink;

use super::plan::{NetConfig, resolve};
use super::session::{Pipes, Session};
use super::view::Shared;
use super::{Command, invites};
use crate::model::{Action, CallStatus};

pub async fn run(
    identity: Identity,
    link: InviteLink,
    config: NetConfig,
    your_name: &str,
    shared: Arc<Shared>,
    pipes: Pipes,
    mut commands: UnboundedReceiver<Command>,
) -> Result<(), String> {
    // A short link names where the invite is, so it is fetched before anything else.
    let invite = match link {
        InviteLink::Full(invite) => invite,
        InviteLink::Short(short) => tokio::select! {
            fetched = fetch_invite(&short, invites::now_unix()) => {
                fetched.map_err(|e| e.to_string())?
            }
            () = wait_for_leave(&mut commands) => return Ok(()),
        },
    };
    // Dual-stack where the system allows it, so a host's IPv6 address is dialled first.
    let node =
        Node::bind_any(&identity, 0).map_err(|e| format!("Cannot open the network: {e}"))?;
    let rendezvous = resolve(config.rendezvous.as_ref(), "rendezvous").await?;
    // Registering with the rendezvous and finding a path to the host can take half a minute
    // over the internet, so Leave is honoured throughout, not only in the lobby.
    let start = async {
        match rendezvous {
            // A configured rendezvous wins over the invite's, for networks the host's cannot
            // reach.
            Some(addr) => CallNode::for_invite_via(&identity, node, &invite, Some(addr)).await,
            None => CallNode::for_invite(&identity, node, &invite).await,
        }
    };
    let mut call = tokio::select! {
        call = start => call,
        () = wait_for_leave(&mut commands) => return Ok(()),
    };
    // Before joining, so the join and the media session made after it both follow it.
    if config.relay_only {
        crate::note!("call: hiding your address, relay only");
        call.set_relay_only(true);
    }
    // The status stays Connecting ("Reaching the host") until the host has the knock.
    let knocked = || shared.update(|view| view.status = CallStatus::Lobby);
    let mut member = tokio::select! {
        joined = join_with(&call, &invite, knocked) => joined.map_err(|e| join_error(&e))?,
        () = wait_for_leave(&mut commands) => return Ok(()),
    };
    shared.update(|view| view.status = CallStatus::Live);
    let media = call.media().map_err(|e| e.to_string())?;
    let mut session = Session::new(media, &identity, invite.host, your_name, shared.clone(), pipes);
    let mut held = Held::default();
    loop {
        // What arrived during a link-up comes first, so nothing overtakes it.
        let flow = if let Some(event) = held.events.pop_front() {
            on_event(event, &mut session, &mut member.events, &mut commands, &mut held).await?
        } else if let Some(text) = held.chat.pop_front() {
            session.send_chat(&text).await;
            Flow::Stay
        } else {
            tokio::select! {
                command = commands.recv() => match command {
                    None | Some(Command::Leave) => Flow::Leave,
                    Some(Command::Act(Action::Chat(text))) => {
                        session.send_chat(&text).await;
                        Flow::Stay
                    }
                    // Only the host decides; the interface offers a guest nothing else to act on.
                    Some(Command::Act(_)) => Flow::Stay,
                },
                event = member.events.recv() => {
                    let event = event.unwrap_or(MemberEvent::Closed);
                    on_event(event, &mut session, &mut member.events, &mut commands, &mut held)
                        .await?
                }
                () = session.pump() => Flow::Stay,
            }
        };
        if flow == Flow::Leave {
            return Ok(());
        }
    }
}

/// Whether the call goes on for this guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Stay,
    Leave,
}

/// What arrived during a link-up, in order. Both channels are already unbounded, so holding
/// these here adds no new way to use up memory.
#[derive(Default)]
struct Held {
    events: VecDeque<MemberEvent>,
    chat: VecDeque<String>,
}

/// The reason to show if `event` ends the call for this guest.
fn ending(event: &MemberEvent) -> Option<String> {
    match event {
        MemberEvent::Removed => Some("The host removed you from the call.".into()),
        MemberEvent::Failed(e) => Some(format!("The call failed: {e}")),
        MemberEvent::Closed => Some("The host ended the call.".into()),
        MemberEvent::Keys(_) | MemberEvent::Routes(_) | MemberEvent::PlanRefused(_) => None,
    }
}

async fn on_event(
    event: MemberEvent,
    session: &mut Session,
    events: &mut UnboundedReceiver<MemberEvent>,
    commands: &mut UnboundedReceiver<Command>,
    held: &mut Held,
) -> Result<Flow, String> {
    if let Some(reason) = ending(&event) {
        return Err(reason);
    }
    match event {
        MemberEvent::Keys(keys) => session.on_keys(keys),
        MemberEvent::Routes(routes) => {
            return link_up(session.on_routes(routes), events, commands, held).await;
        }
        MemberEvent::PlanRefused(e) => crate::note!("refused the host's plan: {e}"),
        // `ending` has already returned for these.
        MemberEvent::Removed | MemberEvent::Closed | MemberEvent::Failed(_) => {}
    }
    Ok(Flow::Stay)
}

/// Waits for `linking` while still listening. The end of the call and Leave stop the wait,
/// which drops any dial in progress. Anything else is kept for afterwards.
async fn link_up(
    linking: impl Future<Output = ()>,
    events: &mut UnboundedReceiver<MemberEvent>,
    commands: &mut UnboundedReceiver<Command>,
    held: &mut Held,
) -> Result<Flow, String> {
    tokio::pin!(linking);
    loop {
        tokio::select! {
            () = &mut linking => return Ok(Flow::Stay),
            event = events.recv() => {
                let event = event.unwrap_or(MemberEvent::Closed);
                if let Some(reason) = ending(&event) {
                    return Err(reason);
                }
                held.events.push_back(event);
            }
            command = commands.recv() => match command {
                None | Some(Command::Leave) => return Ok(Flow::Leave),
                Some(Command::Act(Action::Chat(text))) => held.chat.push_back(text),
                Some(Command::Act(_)) => {}
            },
        }
    }
}

async fn wait_for_leave(commands: &mut UnboundedReceiver<Command>) {
    while let Some(command) = commands.recv().await {
        if matches!(command, Command::Leave) {
            return;
        }
    }
}

fn join_error(e: &CallError) -> String {
    match e {
        CallError::Refused(RefuseReason::DeclinedByHost) => {
            String::from("The host did not let you in.")
        }
        // The host gives out a fresh link after each knock, so an old one stops working.
        CallError::Refused(RefuseReason::TokenUsed) => String::from(
            "This invite link was already used or has expired. Ask the host for a new one.",
        ),
        CallError::Refused(RefuseReason::UnknownRoom) => {
            String::from("This invite is for a call that has ended.")
        }
        CallError::Refused(RefuseReason::RoomFull) => String::from("The call is full."),
        CallError::Timeout => String::from("The host did not answer."),
        // Windows blocks inbound UDP for an app with no firewall rule, which looks exactly
        // like this from the guest's side; the host's own invite lists only its addresses.
        CallError::Net(_) | CallError::Rendezvous(_) => format!(
            "Could not reach the host. Its firewall may be blocking Veil: the host can press \
             Allow guests before starting the call. ({e})"
        ),
        other => format!("Could not reach the call: {other}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::future::pending;
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

    use super::*;

    type Channels = (
        UnboundedSender<MemberEvent>,
        UnboundedReceiver<MemberEvent>,
        UnboundedSender<Command>,
        UnboundedReceiver<Command>,
    );

    fn channels() -> Channels {
        let (event_tx, events) = unbounded_channel();
        let (command_tx, commands) = unbounded_channel();
        (event_tx, events, command_tx, commands)
    }

    #[tokio::test]
    async fn the_host_ending_the_call_interrupts_a_link_up() {
        let (event_tx, mut events, _command_tx, mut commands) = channels();
        event_tx.send(MemberEvent::Closed).unwrap();
        let result = link_up(pending(), &mut events, &mut commands, &mut Held::default()).await;
        assert_eq!(result, Err("The host ended the call.".to_string()));
    }

    #[tokio::test]
    async fn being_removed_interrupts_a_link_up() {
        let (event_tx, mut events, _command_tx, mut commands) = channels();
        event_tx.send(MemberEvent::Removed).unwrap();
        let result = link_up(pending(), &mut events, &mut commands, &mut Held::default()).await;
        assert_eq!(result, Err("The host removed you from the call.".to_string()));
    }

    #[tokio::test]
    async fn leaving_interrupts_a_link_up() {
        let (_event_tx, mut events, command_tx, mut commands) = channels();
        command_tx.send(Command::Leave).map_err(|_| "closed").unwrap();
        let result = link_up(pending(), &mut events, &mut commands, &mut Held::default()).await;
        assert_eq!(result, Ok(Flow::Leave));
    }

    #[tokio::test]
    async fn other_arrivals_wait_in_order_until_the_link_up_ends() {
        let (event_tx, mut events, command_tx, mut commands) = channels();
        event_tx.send(MemberEvent::PlanRefused(CallError::Closed)).unwrap();
        event_tx.send(MemberEvent::PlanRefused(CallError::Timeout)).unwrap();
        for text in ["first", "second"] {
            let chat = Command::Act(Action::Chat(text.into()));
            command_tx.send(chat).map_err(|_| "closed").unwrap();
        }
        let mut held = Held::default();
        let linking = tokio::time::sleep(Duration::from_millis(50));
        let result = link_up(linking, &mut events, &mut commands, &mut held).await;
        assert_eq!(result, Ok(Flow::Stay));
        assert!(matches!(
            held.events.make_contiguous(),
            [
                MemberEvent::PlanRefused(CallError::Closed),
                MemberEvent::PlanRefused(CallError::Timeout)
            ]
        ));
        assert_eq!(held.chat, ["first", "second"]);
    }

    #[test]
    fn join_errors_read_plainly() {
        assert_eq!(join_error(&CallError::Timeout), "The host did not answer.");
        assert!(join_error(&CallError::Closed).starts_with("Could not reach the call"));
        let unreachable = CallError::Net(veil_net::NetError::Timeout);
        let text = join_error(&unreachable);
        assert!(text.contains("press Allow guests"), "one space at the line join: {text}");
    }

    #[test]
    fn a_used_invite_is_not_blamed_on_the_host() {
        let used = join_error(&CallError::Refused(RefuseReason::TokenUsed));
        assert!(used.contains("Ask the host for a new one"), "{used}");
        let declined = join_error(&CallError::Refused(RefuseReason::DeclinedByHost));
        assert_eq!(declined, "The host did not let you in.");
    }
}
