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

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallError, CallNode, MemberEvent, join_with};
use veil_net::Node;
use veil_security::Identity;
use veil_security::invite::Invite;

use super::Command;
use super::plan::NetConfig;
use super::session::{Pipes, Session};
use super::view::Shared;
use crate::model::{Action, CallStatus};

pub async fn run(
    identity: Identity,
    invite: Invite,
    config: NetConfig,
    your_name: &str,
    shared: Arc<Shared>,
    pipes: Pipes,
    mut commands: UnboundedReceiver<Command>,
) -> Result<(), String> {
    let bind = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0);
    let node = Node::bind(&identity, bind).map_err(|e| format!("Cannot open the network: {e}"))?;
    let rendezvous = config.rendezvous;
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
    // The status stays Connecting ("Reaching the host") until the host has the knock.
    let knocked = || shared.update(|view| view.status = CallStatus::Lobby);
    let mut member = tokio::select! {
        joined = join_with(&call, &invite, knocked) => joined.map_err(|e| join_error(&e))?,
        () = wait_for_leave(&mut commands) => return Ok(()),
    };
    shared.update(|view| view.status = CallStatus::Live);
    let media = call.media().map_err(|e| e.to_string())?;
    let mut session = Session::new(
        media,
        &identity,
        invite.host,
        your_name,
        shared.clone(),
        pipes,
    );
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Leave) => return Ok(()),
                Some(Command::Act(Action::Chat(text))) => session.send_chat(&text).await,
                // Only the host decides; the interface offers a guest nothing else to act on.
                Some(Command::Act(_)) => {}
            },
            event = member.events.recv() => match event {
                Some(MemberEvent::Keys(keys)) => session.on_keys(keys),
                Some(MemberEvent::Routes(routes)) => session.on_routes(routes).await,
                Some(MemberEvent::PlanRefused(e)) => {
                    eprintln!("veil-app: refused the host's plan: {e}");
                }
                Some(MemberEvent::Removed) => return Err("The host removed you from the call.".into()),
                Some(MemberEvent::Failed(e)) => return Err(format!("The call failed: {e}")),
                Some(MemberEvent::Closed) | None => return Err("The host ended the call.".into()),
            },
            () = session.pump() => {}
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
        CallError::Refused(_) => String::from("The host did not let you in."),
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
mod tests {
    use super::*;

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
}
