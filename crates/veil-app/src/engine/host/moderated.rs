//! The host carries out choices: its own from the interface, and moderators' requests that
//! `Session` already checked (`engine/moderation.rs`).
//!
//! Invariants:
//! - A moderator's admit or decline goes through the same lobby decision as the host's own
//!   (`HostHandle::decide`), and only for someone who is waiting now. A remove goes through
//!   `HostHandle::remove`, so MLS removal and a new epoch follow as always.
//! - After every turn of the host loop, moderators learn who waits ([`carry_out`]).

use veil_call::{HostHandle, Moderation};
use veil_security::PublicId;

use crate::engine::session::Session;
use crate::model::Action;

/// Carries out one choice from the interface.
pub(super) async fn act(
    host: &HostHandle,
    session: &mut Session,
    knocks: &mut Vec<(u32, PublicId)>,
    action: Action,
) {
    let result = match action {
        Action::Admit(id) | Action::Decline(id) => {
            let Some(index) = knocks.iter().position(|(k, _)| *k == id) else {
                return;
            };
            let (_, device) = knocks.remove(index);
            host.decide(device, matches!(action, Action::Admit(_)))
                .await
        }
        Action::Lobby(on) => host.set_gate(on).await,
        Action::Remove(id) => match session.ids.device(id) {
            Some(device) => host.remove(device).await,
            None => return,
        },
        Action::Mute(id) => {
            session.ask_mute(id).await;
            return;
        }
        Action::Moderator(id, on) => {
            session.set_moderator(id, on).await;
            return;
        }
        // The session sends chat (see `run`); the engine saves verified keys.
        Action::Chat(..)
        | Action::React(..)
        | Action::Hand(_)
        | Action::Verify(..)
        | Action::InviteUntilEnd(_) => {
            return;
        }
    };
    if let Err(e) = result {
        crate::note!("{action:?} failed: {e}");
    }
}

/// Carries out moderators' checked requests, then tells moderators who waits now.
pub(super) async fn carry_out(
    host: &HostHandle,
    session: &mut Session,
    knocks: &mut Vec<(u32, PublicId)>,
) {
    for request in session.take_requests() {
        match request {
            Moderation::Admit(device) | Moderation::Decline(device) => {
                let device = PublicId(device);
                let Some(&(id, _)) = knocks.iter().find(|(_, d)| *d == device) else {
                    continue;
                };
                let action = match request {
                    Moderation::Admit(_) => Action::Admit(id),
                    _ => Action::Decline(id),
                };
                act(host, session, knocks, action).await;
            }
            Moderation::Remove(device) => {
                if let Err(e) = host.remove(PublicId(device)).await {
                    crate::note!("a moderator's remove failed: {e}");
                }
            }
        }
    }
    session.set_waiting(knocks.iter().map(|(_, d)| *d).collect());
}
