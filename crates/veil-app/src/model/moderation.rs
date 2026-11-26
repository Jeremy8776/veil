//! A call's moderation as the interface sees it: who moderates, and asking someone to mute.
//!
//! Purpose: the host names moderators, who can admit, decline, remove and mute like the host
//! (`engine/moderation.rs` holds the rules and checks). These methods only queue the choice
//! for the engine, or in a demo act on the simulation at once.
//!
//! Invariants:
//! - Only the host names or drops moderators. Nobody names or removes the host, or acts on
//!   themselves.
//! - A mute asks the other person's Veil to mute their microphone; they can unmute. It is not
//!   the "Mute for me" volume, which never leaves this device (`volume.rs`).

use super::{Action, Call};

impl Call {
    /// You may admit, decline, remove and mute: the host, or a moderator the host named.
    pub fn can_moderate(&self) -> bool {
        self.is_host || self.you_moderate
    }

    pub fn is_moderator(&self, id: u32) -> bool {
        self.moderators.contains(&id)
    }

    /// Someone else in the call who is not the host: the people moderation can act on.
    pub(crate) fn others_not_host(&self, id: u32) -> bool {
        self.participants
            .iter()
            .any(|p| p.id == id && !p.is_you && !p.is_host)
    }

    /// Asks person `id` to mute their microphone, for everyone.
    pub fn mute_for_everyone(&mut self, id: u32) {
        if !self.can_moderate() || !self.others_not_host(id) {
            return;
        }
        let Some(person) = self
            .participants
            .iter_mut()
            .find(|p| p.id == id && !p.is_you)
        else {
            return;
        };
        if self.demo {
            person.muted = true;
            person.speaking = false;
        } else {
            self.outbox.push(Action::Mute(id));
        }
    }

    /// Names person `id` a moderator (`on`) or drops them. Host only.
    pub fn set_moderator(&mut self, id: u32, on: bool) {
        if !self.is_host || !self.others_not_host(id) || self.is_moderator(id) == on {
            return;
        }
        if on {
            self.moderators.push(id);
        } else {
            self.moderators.retain(|m| *m != id);
        }
        if !self.demo {
            self.outbox.push(Action::Moderator(id, on));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Instant;

    use super::super::Participant;
    use super::*;

    /// Someone else in `call`, copied from your own entry.
    fn someone(call: &Call, id: u32) -> Participant {
        Participant {
            id,
            is_you: false,
            is_host: false,
            ..call.participants[0].clone()
        }
    }

    #[test]
    fn only_the_host_names_moderators() {
        let mut call = crate::demo::call("Me", true, Instant::now());
        let guest = call.participants.iter().find(|p| !p.is_you).map(|p| p.id);
        let guest = guest.unwrap_or_default();
        call.set_moderator(guest, true);
        assert!(call.is_moderator(guest));
        let mut as_guest = crate::demo::call("Me", false, Instant::now());
        as_guest.set_moderator(guest, true);
        assert!(!as_guest.is_moderator(guest));
        assert!(!as_guest.can_moderate());
        as_guest.you_moderate = true;
        assert!(as_guest.can_moderate());
    }

    #[test]
    fn a_live_call_queues_moderation_for_the_engine() {
        let mut call = Call::live("Call".into(), "Host", true, Instant::now());
        let guest = someone(&call, 4);
        call.participants.push(guest);
        call.mute_for_everyone(4);
        call.set_moderator(4, true);
        call.set_moderator(4, true);
        assert_eq!(call.outbox, [Action::Mute(4), Action::Moderator(4, true)]);
        let you = call.you().map(|p| p.id).unwrap_or_default();
        call.mute_for_everyone(you);
        assert_eq!(call.outbox.len(), 2, "not yourself");
    }

    #[test]
    fn a_moderator_cannot_act_on_the_host() {
        let mut call = Call::live("Call".into(), "Me", false, Instant::now());
        call.you_moderate = true;
        let mut host = someone(&call, 1);
        host.is_host = true;
        call.participants.push(host);
        call.mute_for_everyone(1);
        call.remove(1);
        assert!(call.outbox.is_empty());
    }
}
