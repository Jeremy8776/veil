//! Moderators: people the host lets admit, decline, remove and mute, and the checks on every
//! moderation message (`veil_call::AppMessage::{Mute, Moderators, Waiting, Moderate}`).
//!
//! Purpose: the host names moderators; this device's [`Roles`] keeps who moderates, decides
//! whether a message it received carries authority, and says what the host must send next.
//! The session (`session.rs`) feeds it signed senders and sends what [`Roles::outgoing`]
//! returns. It is pure, so every rule below is tested without a call.
//!
//! Invariants:
//! - Authority comes from the signed sender only. A moderator list or a lobby list counts only
//!   from the pinned host; a mute counts only from the host or a listed moderator, and only
//!   when it names this device; a request counts only on the host, and only from a moderator.
//! - Moderators never act themselves. Admit, decline and remove go to the host as requests,
//!   which the host carries out through its own lobby and removal path (`host.rs`), so the
//!   lobby, identity checks and MLS removal apply exactly as for the host's own choice.
//! - Nobody can remove the host. Only the host names or drops moderators. A moderator who
//!   leaves the roster stops being one.
//! - Privacy: the lobby list goes to every member (app messages are sealed for the whole
//!   group), so the host sends it only while someone moderates. It holds device keys only.

use veil_call::{AppMessage, MAX_LISTED_WAITING, MAX_MODERATORS, Moderation};
use veil_security::PublicId;

/// What a received moderation message means for this device.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Not moderation, or no authority: nothing to do.
    None,
    /// Someone with authority asked this device to mute its microphone.
    MuteYou(PublicId),
    /// Who moderates changed.
    Moderators,
    /// The host's lobby, for a moderator to show.
    Waiting(Vec<PublicId>),
    /// On the host: a moderator's request, already checked, to carry out.
    Request(Moderation),
}

pub struct Roles {
    host: PublicId,
    me: PublicId,
    moderators: Vec<PublicId>,
    /// On the host: who waits now, and the list last sent.
    waiting: Vec<PublicId>,
    sent_waiting: Option<Vec<PublicId>>,
    /// On the host: the moderator list must go out (it changed, or a new epoch began).
    resend: bool,
}

impl Roles {
    pub fn new(host: PublicId, me: PublicId) -> Self {
        Self {
            host,
            me,
            moderators: Vec::new(),
            waiting: Vec::new(),
            sent_waiting: None,
            resend: false,
        }
    }

    fn is_host(&self) -> bool {
        self.me == self.host
    }

    pub fn moderators(&self) -> &[PublicId] {
        &self.moderators
    }

    pub fn is_moderator(&self, device: &PublicId) -> bool {
        self.moderators.contains(device)
    }

    /// This device may admit, decline, remove and mute: the host, or a moderator.
    pub fn you_moderate(&self) -> bool {
        self.is_host() || self.is_moderator(&self.me)
    }

    /// On the host: names `device` a moderator (`on`) or drops it. Returns whether anything
    /// changed. The host itself, and more than [`MAX_MODERATORS`], are refused.
    pub fn set(&mut self, device: PublicId, on: bool) -> bool {
        if !self.is_host() || device == self.host {
            return false;
        }
        let had = self.is_moderator(&device);
        if on && !had && self.moderators.len() < MAX_MODERATORS {
            self.moderators.push(device);
        } else if !on && had {
            self.moderators.retain(|d| *d != device);
        } else {
            return false;
        }
        self.resend = true;
        true
    }

    /// A new epoch with these members: moderators who left stop being ones, and the host
    /// sends its list again so a member who just joined learns it.
    pub fn epoch(&mut self, roster: &[PublicId]) {
        self.moderators.retain(|d| roster.contains(d));
        if self.is_host() {
            self.resend = true;
            self.sent_waiting = None;
        }
    }

    /// On the host: who waits in its lobby now, oldest first.
    pub fn set_waiting(&mut self, waiting: Vec<PublicId>) {
        self.waiting = waiting;
    }

    /// On the host: the moderation messages due now. Empty on everyone else.
    pub fn outgoing(&mut self) -> Vec<AppMessage> {
        if !self.is_host() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if std::mem::take(&mut self.resend) {
            out.push(AppMessage::Moderators(
                self.moderators.iter().map(|d| d.0).collect(),
            ));
        }
        // The lobby goes out only while someone moderates; an empty list tells them it emptied.
        let listed: Vec<PublicId> = if self.moderators.is_empty() {
            Vec::new()
        } else {
            self.waiting
                .iter()
                .take(MAX_LISTED_WAITING)
                .copied()
                .collect()
        };
        let known = self.sent_waiting.as_ref();
        if known != Some(&listed) && (!listed.is_empty() || known.is_some_and(|k| !k.is_empty())) {
            out.push(AppMessage::Waiting(listed.iter().map(|d| d.0).collect()));
        }
        self.sent_waiting = Some(listed);
        out
    }

    /// What `message`, signed by `from`, means here.
    pub fn receive(&mut self, from: PublicId, message: &AppMessage) -> Effect {
        match message {
            AppMessage::Mute(target) => {
                let allowed = from == self.host || self.is_moderator(&from);
                if allowed && PublicId(*target) == self.me {
                    Effect::MuteYou(from)
                } else {
                    Effect::None
                }
            }
            AppMessage::Moderators(list) if from == self.host && !self.is_host() => {
                let list: Vec<PublicId> = list
                    .iter()
                    .map(|d| PublicId(*d))
                    .filter(|d| *d != self.host)
                    .collect();
                if list == self.moderators {
                    return Effect::None;
                }
                self.moderators = list;
                Effect::Moderators
            }
            AppMessage::Waiting(list) if from == self.host && !self.is_host() => {
                Effect::Waiting(list.iter().map(|d| PublicId(*d)).collect())
            }
            AppMessage::Moderate(request) if self.is_host() && self.is_moderator(&from) => {
                match request {
                    Moderation::Remove(device) if PublicId(*device) == self.host => Effect::None,
                    _ => Effect::Request(*request),
                }
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: PublicId = PublicId([1; 32]);
    const MOD: PublicId = PublicId([2; 32]);
    const GUEST: PublicId = PublicId([3; 32]);

    #[test]
    fn only_the_host_names_moderators_and_never_itself() {
        let mut host = Roles::new(HOST, HOST);
        assert!(!host.set(HOST, true), "the host is not its own moderator");
        assert!(host.set(MOD, true));
        assert!(!host.set(MOD, true), "already one");
        assert_eq!(host.outgoing(), vec![AppMessage::Moderators(vec![MOD.0])]);
        assert!(host.outgoing().is_empty(), "sent once");
        let mut guest = Roles::new(HOST, GUEST);
        assert!(!guest.set(MOD, true), "a guest names nobody");
        assert!(guest.outgoing().is_empty());
    }

    #[test]
    fn a_moderator_list_counts_only_from_the_host() {
        let mut guest = Roles::new(HOST, GUEST);
        let list = AppMessage::Moderators(vec![GUEST.0]);
        assert_eq!(guest.receive(MOD, &list), Effect::None, "not from the host");
        assert!(!guest.you_moderate());
        assert_eq!(guest.receive(HOST, &list), Effect::Moderators);
        assert!(guest.you_moderate());
        let with_host = AppMessage::Moderators(vec![HOST.0]);
        guest.receive(HOST, &with_host);
        assert!(guest.moderators().is_empty(), "the host is never listed");
    }

    #[test]
    fn a_mute_counts_from_the_host_or_a_moderator_and_only_for_you() {
        let mut guest = Roles::new(HOST, GUEST);
        let mute_you = AppMessage::Mute(GUEST.0);
        assert_eq!(
            guest.receive(MOD, &mute_you),
            Effect::None,
            "not a moderator yet"
        );
        guest.receive(HOST, &AppMessage::Moderators(vec![MOD.0]));
        assert_eq!(guest.receive(MOD, &mute_you), Effect::MuteYou(MOD));
        assert_eq!(guest.receive(HOST, &mute_you), Effect::MuteYou(HOST));
        let other = AppMessage::Mute(MOD.0);
        assert_eq!(guest.receive(HOST, &other), Effect::None, "someone else");
    }

    #[test]
    fn the_host_takes_requests_from_moderators_only_and_cannot_be_removed() {
        let mut host = Roles::new(HOST, HOST);
        let admit = AppMessage::Moderate(Moderation::Admit(GUEST.0));
        assert_eq!(host.receive(MOD, &admit), Effect::None, "not a moderator");
        host.set(MOD, true);
        assert_eq!(
            host.receive(MOD, &admit),
            Effect::Request(Moderation::Admit(GUEST.0))
        );
        let remove_host = AppMessage::Moderate(Moderation::Remove(HOST.0));
        assert_eq!(host.receive(MOD, &remove_host), Effect::None);
        let mut guest = Roles::new(HOST, GUEST);
        assert_eq!(
            guest.receive(MOD, &admit),
            Effect::None,
            "only the host acts"
        );
    }

    #[test]
    fn the_lobby_goes_out_only_while_someone_moderates() {
        let mut host = Roles::new(HOST, HOST);
        host.set_waiting(vec![GUEST]);
        assert!(host.outgoing().is_empty(), "nobody moderates");
        host.set(MOD, true);
        let out = host.outgoing();
        assert!(out.contains(&AppMessage::Waiting(vec![GUEST.0])));
        assert!(host.outgoing().is_empty(), "unchanged");
        host.set_waiting(Vec::new());
        assert_eq!(host.outgoing(), vec![AppMessage::Waiting(Vec::new())]);
        host.set(MOD, false);
        assert_eq!(host.outgoing(), vec![AppMessage::Moderators(Vec::new())]);
    }

    #[test]
    fn a_moderator_who_leaves_stops_being_one() {
        let mut host = Roles::new(HOST, HOST);
        host.set(MOD, true);
        host.outgoing();
        host.epoch(&[HOST, GUEST]);
        assert!(!host.is_moderator(&MOD));
        assert_eq!(host.outgoing(), vec![AppMessage::Moderators(Vec::new())]);
    }
}
