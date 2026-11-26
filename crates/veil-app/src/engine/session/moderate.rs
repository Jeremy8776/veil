//! The session's side of moderation: sends what the host or a moderator chose, and shows what
//! arrives. The rules and checks live in `engine/moderation.rs` ([`Roles`]).
//!
//! Invariants:
//! - Every received moderation message goes through [`Roles::receive`] with its signed sender
//!   first; this module only acts on the [`Effect`] it returns.
//! - A moderator's admit, decline and remove leave as requests to the host; only the host's
//!   own lobby and removal path (`host.rs`) carries them out.
//! - A guest shows the host's lobby only while it moderates.

use std::time::Instant;

use veil_call::{AppMessage, Moderation};
use veil_security::PublicId;

use super::Session;
use crate::engine::moderation::{Effect, Roles};
use crate::engine::view::{Knocker, Said};
use crate::model::Action;

impl Session {
    pub(super) fn roles(host: PublicId, me: PublicId) -> Roles {
        Roles::new(host, me)
    }

    /// Host: names person `id` a moderator (`on`) or drops them, and tells everyone.
    pub async fn set_moderator(&mut self, id: u32, on: bool) {
        let Some(device) = self.ids.device(id) else {
            return;
        };
        if self.roles.set(device, on) {
            self.show_roles();
            self.send_moderation().await;
        }
    }

    /// Host or moderator: asks person `id` to mute their microphone.
    pub async fn ask_mute(&mut self, id: u32) {
        if !self.roles.you_moderate() {
            return;
        }
        if let Some(device) = self.ids.device(id) {
            self.send_app(&AppMessage::Mute(device.0)).await;
        }
    }

    /// Moderator: asks the host to admit, decline or remove someone. Anything else is ignored.
    pub async fn request(&mut self, action: &Action) {
        if !self.roles.you_moderate() {
            return;
        }
        let (Action::Admit(id) | Action::Decline(id) | Action::Remove(id)) = action else {
            return;
        };
        let Some(device) = self.ids.device(*id) else {
            return;
        };
        let request = match action {
            Action::Admit(_) => Moderation::Admit(device.0),
            Action::Decline(_) => Moderation::Decline(device.0),
            _ => Moderation::Remove(device.0),
        };
        self.send_app(&AppMessage::Moderate(request)).await;
    }

    /// Host: who waits in the lobby now, oldest first; moderators hear of changes.
    pub fn set_waiting(&mut self, waiting: Vec<PublicId>) {
        self.roles.set_waiting(waiting);
    }

    /// Host: moderators' requests that passed the checks, to carry out.
    pub fn take_requests(&mut self) -> Vec<Moderation> {
        std::mem::take(&mut self.requests)
    }

    /// A moderation message `from` signed. Anything without authority is dropped.
    pub(super) fn on_moderation(&mut self, from: PublicId, message: &AppMessage) {
        match self.roles.receive(from, message) {
            Effect::None => {}
            Effect::MuteYou(_) => self.shared.push_said(from, Said::MuteYou),
            Effect::Moderators => self.show_roles(),
            Effect::Waiting(devices) => self.show_waiting(&devices),
            Effect::Request(request) => self.requests.push(request),
        }
    }

    /// A new epoch with these members.
    pub(super) fn roles_epoch(&mut self, roster: &[PublicId]) {
        self.roles.epoch(roster);
        self.show_roles();
    }

    /// Sends what the host's moderation owes everyone now.
    pub(super) async fn send_moderation(&mut self) {
        for message in self.roles.outgoing() {
            self.send_app(&message).await;
        }
    }

    fn show_roles(&self) {
        let moderators = self.roles.moderators().to_vec();
        let guest = self.me != self.host;
        let you = guest && self.roles.you_moderate();
        self.shared.update(|view| {
            view.moderators = moderators;
            view.you_moderate = you;
            // A guest that stops moderating stops seeing the host's lobby.
            if guest && !you {
                view.lobby.clear();
            }
        });
    }

    /// A moderating guest shows the host's lobby, keeping how long each has waited.
    fn show_waiting(&mut self, devices: &[PublicId]) {
        if !self.roles.you_moderate() {
            return;
        }
        let now = Instant::now();
        let known: Vec<(PublicId, Instant)> = {
            let view = crate::media::lock(&self.shared.view);
            view.lobby.iter().map(|k| (k.device, k.since)).collect()
        };
        let lobby: Vec<Knocker> = devices
            .iter()
            .map(|device| Knocker {
                id: self.ids.id(*device),
                device: *device,
                name: self.name_for(device),
                safety: self.safety_with(device),
                since: known
                    .iter()
                    .find(|(d, _)| d == device)
                    .map_or(now, |(_, since)| *since),
            })
            .collect();
        self.shared.update(|view| view.lobby = lobby);
    }
}
