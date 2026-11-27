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

use tokio::sync::mpsc::{Receiver, UnboundedReceiver};
use veil_call::{CallError, CallNode, MemberEvent, join_with};
use veil_net::Node;
use veil_net::control::RefuseReason;
use veil_rendezvous::fetch_invite;
use veil_security::Identity;
use veil_security::short_invite::InviteLink;

use super::helper_evidence::HelperWatch;
use super::plan::{NetConfig, resolve};
use super::session::{Pipes, Session};
use super::view::Shared;
use super::{Command, invites};
use crate::diagnostics::{Problem, State, Step};
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
    let config = config.enforce_policy();
    if let Some(why) = config.conflict() {
        shared.diagnose(|d| d.set(Step::Socket, State::Failed(Problem::Settings)));
        return Err(why.into());
    }
    // A short link names where the invite is, so it is fetched before anything else.
    let invite = match link {
        InviteLink::Full(invite) => invite,
        InviteLink::Short(_) if config.local_only => {
            shared.diagnose(|d| d.set(Step::Invitation, State::Failed(Problem::NoRoute)));
            return Err(String::from(
                "Local only needs the full invitation. Ask the host to use Copy full link or show its QR code.",
            ));
        }
        InviteLink::Short(short) => tokio::select! {
            fetched = fetch_invite(&short, invites::now_unix()) => {
                fetched.map_err(|e| {
                    shared.diagnose(|d| d.set(Step::Invitation, State::Failed(match e {
                        veil_rendezvous::FetchError::Refused(_) => Problem::InviteExpired,
                        _ => Problem::InviteUnavailable,
                    })));
                    e.to_string()
                })?
            }
            () = wait_for_leave(&mut commands) => return Ok(()),
        },
    };
    shared.diagnose(|d| {
        d.set(Step::Invitation, State::Passed);
        d.set(Step::Hosting, State::Skipped);
        for addr in invite.addrs.iter().filter_map(|a| a.parse().ok()) {
            d.endpoint(
                false,
                veil_net::NodeAddr {
                    key: invite.host,
                    addr,
                },
            );
        }
    });
    // Dual-stack where the system allows it, so a host's IPv6 address is dialled first.
    let node = if config.local_only {
        Node::bind_local(&identity, 0)
    } else {
        Node::bind_any(&identity, 0)
    }
    .map_err(|e| {
        shared.diagnose(|d| d.set(Step::Socket, State::Failed(Problem::net(&e))));
        format!("Cannot open the network: {e}")
    })?;
    shared.diagnose(|d| d.set(Step::Socket, State::Passed));
    // A rendezvous you chose is not swapped for the host's when it cannot be found: that
    // node would see your address, which matters most with Hide my address. Local only has
    // none configured (`enforce_policy`), so this never blocks a local call.
    let rendezvous = resolve(config.rendezvous.as_ref(), "rendezvous")
        .await
        .inspect_err(|_| {
            shared.diagnose(|d| d.set(Step::Helper, State::Failed(Problem::HelperUnavailable)));
        })?;
    shared.diagnose(|d| {
        d.set(Step::Helper, State::Waiting);
        if let Some(addr) = rendezvous {
            d.endpoint(true, addr);
        } else if !config.local_only
            && let Some(relay) = invite
                .relay
                .as_deref()
                .and_then(veil_call::RelayAddrs::parse)
        {
            for addr in relay.addrs {
                d.endpoint(
                    true,
                    veil_net::NodeAddr {
                        key: relay.key,
                        addr,
                    },
                );
            }
        }
    });
    // Registering with the rendezvous and finding a path to the host can take half a minute
    // over the internet, so Leave is honoured throughout, not only in the lobby.
    let start = async {
        match rendezvous {
            _ if config.local_only => {
                // Full invite stays signed and unchanged; this override bypasses even an
                // internet helper supplied inside it. Peer addresses arrive over the
                // authenticated host connection after admission.
                CallNode::for_invite_via(&identity, node, &invite, None).await
            }
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
    let helper_needed = !config.local_only && (rendezvous.is_some() || invite.relay.is_some());
    let mut helper_watch = HelperWatch::start(&call, helper_needed, false);
    shared.diagnose(|d| {
        helper_watch.write(d);
        d.set(Step::HostConnection, State::Waiting);
    });
    // Before joining, so the join and the media session made after it both follow it.
    if config.relay_only {
        crate::note!("call: hiding your address, relay only");
        call.set_relay_only(true);
    }
    // The status stays Connecting ("Reaching the host") until the host has the knock.
    let knocked = || {
        shared.diagnose(|d| {
            d.set(Step::HostConnection, State::Passed);
            d.set(Step::Admission, State::Waiting);
        });
        shared.update(|view| view.status = CallStatus::Lobby);
    };
    let mut member = tokio::select! {
        joined = join_with(&call, &invite, knocked) => joined.map_err(|e| {
            shared.diagnose(|d| {
                let step = if d.steps[Step::HostConnection as usize] == State::Passed { Step::Admission } else { Step::HostConnection };
                d.set(step, State::Failed(Problem::call(&e)));
            });
            join_error(&e)
        })?,
        () = wait_for_leave(&mut commands) => return Ok(()),
    };
    shared.update(|view| view.status = CallStatus::Live);
    shared.diagnose(|d| {
        d.set(Step::Admission, State::Passed);
        d.set(Step::Membership, State::Passed);
    });
    let media = call.media().map_err(|e| e.to_string())?;
    let mut session = Session::new(
        media,
        &identity,
        invite.host,
        your_name,
        shared.clone(),
        pipes,
    );
    let mut held = Held::default();
    // The presence keeps retrying a helper that blipped at join; keep its row current.
    let mut helper_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        // What arrived during a link-up comes first, so nothing overtakes it.
        let flow = if let Some(event) = held.events.pop_front() {
            on_event(
                event,
                &mut session,
                &mut member.events,
                &mut commands,
                &mut held,
            )
            .await?
        } else if let Some(said) = held.chat.pop_front() {
            session.say(&said).await;
            Flow::Stay
        } else if let Some(raised) = held.hand.take() {
            session.set_hand(raised).await;
            Flow::Stay
        } else {
            tokio::select! {
                command = commands.recv() => match command {
                    None | Some(Command::Leave) => Flow::Leave,
                    Some(Command::Act(said @ (Action::Chat(..) | Action::React(..)))) => {
                        session.say(&said).await;
                        Flow::Stay
                    }
                    Some(Command::Act(Action::Hand(raised))) => {
                        session.set_hand(raised).await;
                        Flow::Stay
                    }
                    // A moderator asks the host; the session drops it unless the host named you.
                    Some(Command::Act(Action::Mute(id))) => {
                        session.ask_mute(id).await;
                        Flow::Stay
                    }
                    Some(Command::Act(
                        action @ (Action::Admit(_) | Action::Decline(_) | Action::Remove(_)),
                    )) => {
                        session.request(&action).await;
                        Flow::Stay
                    }
                    // Only the host decides the rest.
                    Some(Command::Act(_)) => Flow::Stay,
                },
                event = member.events.recv() => {
                    let event = event.unwrap_or(MemberEvent::Closed);
                    on_event(event, &mut session, &mut member.events, &mut commands, &mut held)
                        .await?
                }
                () = session.pump() => Flow::Stay,
                _ = helper_tick.tick() => {
                    helper_watch.refresh(&shared, &call);
                    Flow::Stay
                }
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

/// Link-up buffering has an independent limit: draining a bounded input must not move an
/// unlimited stream of events into this deque. Overflow ends the call without skipping keys.
const HELD_EVENTS: usize = veil_call::EVENT_BACKLOG;
const HELD_CHAT: usize = 64;
const LINK_UP_OVERLOAD: &str =
    "The call became too busy while connecting. Join again with a new invite.";

/// What arrived during a link-up, in order, with bounded count and chat payload size.
#[derive(Default)]
struct Held {
    events: VecDeque<MemberEvent>,
    /// Chat and reactions, as [`Action::Chat`] and [`Action::React`].
    chat: VecDeque<Action>,
    /// Only the newest hand state matters.
    hand: Option<bool>,
}

impl Held {
    fn event(&mut self, event: MemberEvent) -> Result<(), String> {
        // Terminal events always interrupt, even if the holding queue is already full.
        if let Some(reason) = ending(&event) {
            return Err(reason);
        }
        if self.events.len() >= HELD_EVENTS {
            return Err(LINK_UP_OVERLOAD.into());
        }
        self.events.push_back(event);
        Ok(())
    }

    fn said(&mut self, said: Action) -> Result<(), String> {
        // Validate before retaining a String. These are the same limits as transmission,
        // including multibyte text and reaction lengths.
        let valid = match &said {
            Action::Chat(id, text) => veil_call::AppMessage::chat(*id, text).is_ok(),
            Action::React(id, emoji, on) => veil_call::AppMessage::react(*id, emoji, *on).is_ok(),
            _ => false,
        };
        if self.chat.len() >= HELD_CHAT || !valid {
            return Err(LINK_UP_OVERLOAD.into());
        }
        self.chat.push_back(said);
        Ok(())
    }
}

/// The reason to show if `event` ends the call for this guest.
fn ending(event: &MemberEvent) -> Option<String> {
    match event {
        MemberEvent::Overloaded => {
            Some("The call became too busy to keep up. Join again with a new invite.".into())
        }
        MemberEvent::Removed => Some("The host removed you from the call.".into()),
        MemberEvent::ConnectionLost => {
            Some("The connection to the host could not be restored. Join with a new invite.".into())
        }
        MemberEvent::Failed(e) => Some(format!("The call failed: {e}")),
        MemberEvent::Closed => Some("The host ended the call.".into()),
        MemberEvent::Keys(_)
        | MemberEvent::Routes(_)
        | MemberEvent::PlanRefused(_)
        | MemberEvent::Reconnecting
        | MemberEvent::Reconnected => None,
    }
}

async fn on_event(
    event: MemberEvent,
    session: &mut Session,
    events: &mut Receiver<MemberEvent>,
    commands: &mut UnboundedReceiver<Command>,
    held: &mut Held,
) -> Result<Flow, String> {
    session.diagnose_member(&event);
    if let Some(reason) = ending(&event) {
        return Err(reason);
    }
    match event {
        MemberEvent::Keys(keys) => session.on_keys(keys),
        MemberEvent::Routes(routes) => {
            let evidence = session.evidence();
            let result = link_up(
                session.on_routes(routes),
                events,
                commands,
                held,
                Some(&evidence),
            )
            .await;
            if result.is_err() {
                evidence.diagnose(|d| {
                    if d.end.is_none() {
                        d.set(Step::Membership, State::Failed(Problem::Overloaded));
                    }
                });
            }
            return result;
        }
        MemberEvent::PlanRefused(e) => crate::note!("refused the host's plan: {e}"),
        MemberEvent::Reconnecting => session.reconnecting(true),
        MemberEvent::Reconnected => session.reconnecting(false),
        // `ending` has already returned for these.
        MemberEvent::Overloaded
        | MemberEvent::Removed
        | MemberEvent::Closed
        | MemberEvent::ConnectionLost
        | MemberEvent::Failed(_) => {}
    }
    Ok(Flow::Stay)
}

/// Waits for `linking` while still listening. The end of the call and Leave stop the wait,
/// which drops any dial in progress. Anything else is kept for afterwards.
async fn link_up(
    linking: impl Future<Output = ()>,
    events: &mut Receiver<MemberEvent>,
    commands: &mut UnboundedReceiver<Command>,
    held: &mut Held,
    evidence: Option<&Shared>,
) -> Result<Flow, String> {
    tokio::pin!(linking);
    loop {
        tokio::select! {
            () = &mut linking => return Ok(Flow::Stay),
            event = events.recv() => {
                let event = event.unwrap_or(MemberEvent::Closed);
                if let Some(evidence) = evidence { super::session::diagnose_member(evidence, &event); }
                held.event(event)?;
            }
            command = commands.recv() => match command {
                None | Some(Command::Leave) => return Ok(Flow::Leave),
                Some(Command::Act(said @ (Action::Chat(..) | Action::React(..)))) => {
                    held.said(said)?;
                }
                Some(Command::Act(Action::Hand(raised))) => held.hand = Some(raised),
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
        CallError::Refused(RefuseReason::HostChanged) => {
            String::from("The host changed. Ask for a new link.")
        }
        CallError::Refused(RefuseReason::NeedsUpdate) => {
            String::from("Update Veil to join this call.")
        }
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

    use tokio::sync::mpsc::{Sender, UnboundedSender, channel, unbounded_channel};

    use super::*;

    type Channels = (
        Sender<MemberEvent>,
        Receiver<MemberEvent>,
        UnboundedSender<Command>,
        UnboundedReceiver<Command>,
    );

    fn channels() -> Channels {
        let (event_tx, events) = channel(veil_call::EVENT_BACKLOG + 1);
        let (command_tx, commands) = unbounded_channel();
        (event_tx, events, command_tx, commands)
    }

    #[tokio::test]
    async fn the_host_ending_the_call_interrupts_a_link_up() {
        let (event_tx, mut events, _command_tx, mut commands) = channels();
        event_tx.try_send(MemberEvent::Closed).unwrap();
        let result = link_up(
            pending(),
            &mut events,
            &mut commands,
            &mut Held::default(),
            None,
        )
        .await;
        assert_eq!(result, Err("The host ended the call.".to_string()));
    }

    #[tokio::test]
    async fn being_removed_interrupts_a_link_up() {
        let (event_tx, mut events, _command_tx, mut commands) = channels();
        event_tx.try_send(MemberEvent::Removed).unwrap();
        let result = link_up(
            pending(),
            &mut events,
            &mut commands,
            &mut Held::default(),
            None,
        )
        .await;
        assert_eq!(
            result,
            Err("The host removed you from the call.".to_string())
        );
    }

    #[tokio::test]
    async fn leaving_interrupts_a_link_up() {
        let (_event_tx, mut events, command_tx, mut commands) = channels();
        command_tx
            .send(Command::Leave)
            .map_err(|_| "closed")
            .unwrap();
        let result = link_up(
            pending(),
            &mut events,
            &mut commands,
            &mut Held::default(),
            None,
        )
        .await;
        assert_eq!(result, Ok(Flow::Leave));
    }

    #[tokio::test]
    async fn other_arrivals_wait_in_order_until_the_link_up_ends() {
        let (event_tx, mut events, command_tx, mut commands) = channels();
        event_tx
            .try_send(MemberEvent::PlanRefused(CallError::Closed))
            .unwrap();
        event_tx
            .try_send(MemberEvent::PlanRefused(CallError::Timeout))
            .unwrap();
        for text in ["first", "second"] {
            let chat = Command::Act(Action::Chat(1, text.into()));
            command_tx.send(chat).map_err(|_| "closed").unwrap();
        }
        let mut held = Held::default();
        let linking = tokio::time::sleep(Duration::from_millis(50));
        let result = link_up(linking, &mut events, &mut commands, &mut held, None).await;
        assert_eq!(result, Ok(Flow::Stay));
        assert!(matches!(
            held.events.make_contiguous(),
            [
                MemberEvent::PlanRefused(CallError::Closed),
                MemberEvent::PlanRefused(CallError::Timeout)
            ]
        ));
        assert_eq!(
            held.chat,
            [
                Action::Chat(1, "first".into()),
                Action::Chat(1, "second".into())
            ]
        );
    }

    #[tokio::test]
    async fn a_continuous_event_producer_cannot_grow_link_up_storage() {
        let (event_tx, mut events, _command_tx, mut commands) = channels();
        let producer = tokio::spawn(async move {
            for _ in 0..10_000 {
                if event_tx
                    .send(MemberEvent::PlanRefused(CallError::Timeout))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut held = Held::default();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            link_up(pending(), &mut events, &mut commands, &mut held, None),
        )
        .await
        .unwrap();
        assert_eq!(result, Err(LINK_UP_OVERLOAD.into()));
        assert_eq!(held.events.len(), HELD_EVENTS);
        drop(events);
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn excess_chat_cannot_grow_link_up_storage() {
        let (_event_tx, mut events, command_tx, mut commands) = channels();
        for id in 0..=HELD_CHAT {
            command_tx
                .send(Command::Act(Action::Chat(id as u64, "hello".into())))
                .map_err(|_| "closed")
                .unwrap();
        }
        let mut held = Held::default();
        let result = link_up(pending(), &mut events, &mut commands, &mut held, None).await;
        assert_eq!(result, Err(LINK_UP_OVERLOAD.into()));
        assert_eq!(held.chat.len(), HELD_CHAT);
    }

    #[test]
    fn held_chat_checks_payload_limits_and_terminal_events_always_interrupt() {
        let mut held = Held::default();
        let limit = veil_call::app_msg::MAX_CHAT_CHARS;
        assert!(held.said(Action::Chat(1, "🦀".repeat(limit))).is_ok());
        assert!(held.said(Action::Chat(2, "🦀".repeat(limit + 1))).is_err());
        assert!(
            held.said(Action::React(3, "x".repeat(1_000), true))
                .is_err()
        );
        assert_eq!(held.chat.len(), 1);
        for _ in 0..HELD_EVENTS {
            held.event(MemberEvent::Reconnecting).unwrap();
        }
        assert_eq!(
            held.event(MemberEvent::Removed),
            Err("The host removed you from the call.".into())
        );
        assert!(
            held.event(MemberEvent::Overloaded)
                .unwrap_err()
                .contains("too busy")
        );
        assert_eq!(held.events.len(), HELD_EVENTS);
    }

    #[test]
    fn join_errors_read_plainly() {
        assert_eq!(join_error(&CallError::Timeout), "The host did not answer.");
        assert!(join_error(&CallError::Closed).starts_with("Could not reach the call"));
        let unreachable = CallError::Net(veil_net::NetError::Timeout);
        let text = join_error(&unreachable);
        assert!(
            text.contains("press Allow guests"),
            "one space at the line join: {text}"
        );
    }

    #[test]
    fn a_used_invite_is_not_blamed_on_the_host() {
        let used = join_error(&CallError::Refused(RefuseReason::TokenUsed));
        assert!(used.contains("Ask the host for a new one"), "{used}");
        let declined = join_error(&CallError::Refused(RefuseReason::DeclinedByHost));
        assert_eq!(declined, "The host did not let you in.");
    }
}
