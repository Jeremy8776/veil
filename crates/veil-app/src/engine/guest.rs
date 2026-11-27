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

mod handover;

use handover::Handover;

use super::helper_evidence::HelperWatch;
use super::host::successor;
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
    let mut handover = Handover::new(&config);
    shared.update(|view| {
        view.handover.persistent = member.is_persistent();
        view.handover.hide_address = config.relay_only;
    });
    // The presence keeps retrying a helper that blipped at join; keep its row current.
    let mut helper_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        // What arrived during a link-up comes first, so nothing overtakes it.
        let next = if let Some(event) = held.events.pop_front() {
            Next::Event(event)
        } else if let Some(said) = held.chat.pop_front() {
            session.say(&said).await;
            Next::Flow(Flow::Stay)
        } else if let Some(raised) = held.hand.take() {
            session.set_hand(raised).await;
            Next::Flow(Flow::Stay)
        } else {
            tokio::select! {
                command = commands.recv() => Next::Flow(match command {
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
                    Some(Command::Act(Action::AvailableToHost(on))) => {
                        handover.set_available(on, &member, &shared).await;
                        Flow::Stay
                    }
                    Some(Command::Act(Action::RelayChange(stay))) => {
                        if handover.relay_answer(stay, &call, &member, &mut session, &shared).await {
                            Flow::Stay
                        } else {
                            Flow::Leave
                        }
                    }
                    // Only the host decides the rest.
                    Some(Command::Act(_)) => Flow::Stay,
                }),
                event = member.events.recv() => Next::Event(event.unwrap_or(MemberEvent::Closed)),
                () = session.pump() => Next::Flow(Flow::Stay),
                _ = helper_tick.tick() => {
                    helper_watch.refresh(&shared, &call);
                    handover.tick(&member, &shared).await;
                    Next::Flow(Flow::Stay)
                }
            }
        };
        let flow = match next {
            Next::Flow(flow) => flow,
            Next::Event(MemberEvent::HandoverOffer { request, epoch }) => {
                let room = invite.room_id;
                handover.offered(&identity, &config, &call, room, request, epoch, &member).await;
                Flow::Stay
            }
            Next::Event(MemberEvent::Promoted(promotion)) => {
                let Some(prepared) = handover.prepared.take() else {
                    return Err(LOST.into());
                };
                crate::note!("call: this device hosts the call now");
                drop(member);
                let hosting = successor::take_over(
                    &identity,
                    config.clone(),
                    call,
                    session,
                    *promotion,
                    prepared,
                    shared.clone(),
                )
                .await?;
                return hosting.run(&mut commands).await;
            }
            Next::Event(MemberEvent::HostChanging { host, relay, ask }) => {
                handover.changing(host, relay, ask, &call, &member, &mut session, &shared).await;
                Flow::Stay
            }
            Next::Event(MemberEvent::HostChanged(host)) => {
                let name = session.name_for(&host);
                crate::note!("call: {} is the host now", super::short(&host));
                shared.update(|view| view.handover.notice = Some(format!("{name} is now the host.")));
                Flow::Stay
            }
            Next::Event(event) => {
                on_event(event, &mut session, &mut member.events, &mut commands, &mut held).await?
            }
        };
        if flow == Flow::Leave {
            return Ok(());
        }
    }
}

/// One turn of the guest loop: a flow decided already, or an event still to handle. It lives
/// for one turn only, so its size does not matter.
#[allow(clippy::large_enum_variant)]
enum Next {
    Flow(Flow),
    Event(MemberEvent),
}

/// Shown when the call could not continue while the host changed.
const LOST: &str = "The call was lost while the host changed. Ask for a new link.";

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
        MemberEvent::LostInHandover => Some(LOST.into()),
        MemberEvent::Keys(_)
        | MemberEvent::HandoverOffer { .. }
        | MemberEvent::Promoted(_)
        | MemberEvent::HostChanging { .. }
        | MemberEvent::HostChanged(_)
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
        // The guest loop handles a handover's events before this.
        MemberEvent::HandoverOffer { .. }
        | MemberEvent::Promoted(_)
        | MemberEvent::HostChanging { .. }
        | MemberEvent::HostChanged(_) => {}
        // `ending` has already returned for these.
        MemberEvent::Overloaded
        | MemberEvent::Removed
        | MemberEvent::Closed
        | MemberEvent::ConnectionLost
        | MemberEvent::LostInHandover
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
mod tests;
