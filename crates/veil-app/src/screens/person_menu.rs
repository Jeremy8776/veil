//! The right-click menu on a person: on their tile and on their row in People.
//!
//! Purpose: everyone gets that person's volume and "Mute for me", which change only what this
//! device plays (`volume.rs`). The host and moderators also get "Mute for everyone" and
//! "Remove from call"; the host also names or drops moderators (`model/moderation.rs`).
//!
//! Invariants:
//! - Built from `call_menus` pieces, so it looks like the bar's menus. It opens no pop-up of
//!   its own inside.
//! - Your own tile and row have no menu. Nobody gets moderation controls on the host.
//! - The menu only returns a [`Pick`]; the caller applies it to the call after drawing, so a
//!   person list being drawn is never changed under it.

use eframe::egui::{self, PopupCloseBehavior};

use crate::icons::Icon;
use crate::model::{Call, Participant};
use crate::screens::call_menus;
use crate::screens::stats::short_name;
use crate::volume::{Playback, VOICE_MAX};

const WIDTH: f32 = 240.0;
const VOLUME_HINT: &str = "How loud this person plays for you. Nobody else hears a change.";
const MUTE_ME_HINT: &str = "Silences this person for you only. They are not told.";
const MUTE_ALL_HINT: &str =
    "Mutes their microphone for everyone in the call. They can unmute themselves.";
const REMOVE_HINT: &str =
    "Takes them out of the call. A new key epoch starts, so they cannot decrypt anything new.";
const MODERATOR_HINT: &str =
    "A moderator can admit, decline, remove and mute people, as you can. Not you.";

/// A moderation choice made in the menu, for the caller to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    MuteForEveryone(u32),
    Remove(u32),
    Moderator(u32, bool),
}

/// Opens the menu on a right-click of `response`. Returns the moderation choice made, if any.
pub fn attach(
    response: &egui::Response,
    call: &Call,
    playback: &Playback,
    person: &Participant,
) -> Option<Pick> {
    if person.is_you {
        return None;
    }
    egui::Popup::context_menu(response)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .frame(call_menus::frame())
        .show(|ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing.y = 2.0;
            content(ui, call, playback, person)
        })
        .and_then(|shown| shown.inner)
}

fn content(
    ui: &mut egui::Ui,
    call: &Call,
    playback: &Playback,
    person: &Participant,
) -> Option<Pick> {
    call_menus::section(ui, Icon::People, &short_name(&person.name));
    let mut level = playback.voice_level(person.id);
    if call_menus::volume(ui, "Volume", &mut level.volume, VOICE_MAX, VOLUME_HINT) {
        playback.set_voice_level(person.id, level);
    }
    if call_menus::switch_row(ui, &mut level.muted, "Mute for me")
        .on_hover_text(MUTE_ME_HINT)
        .changed()
    {
        playback.set_voice_level(person.id, level);
    }
    if !call.can_moderate() || person.is_host {
        return None;
    }
    call_menus::divider(ui);
    let mut pick = None;
    if !person.muted
        && call_menus::choice(ui, "Mute for everyone", false)
            .on_hover_text(MUTE_ALL_HINT)
            .clicked()
    {
        pick = Some(Pick::MuteForEveryone(person.id));
        ui.close();
    }
    if call.is_host {
        let mut moderator = call.is_moderator(person.id);
        if call_menus::switch_row(ui, &mut moderator, "Moderator")
            .on_hover_text(MODERATOR_HINT)
            .changed()
        {
            pick = Some(Pick::Moderator(person.id, moderator));
        }
    }
    if call_menus::choice(ui, "Remove from call", false)
        .on_hover_text(REMOVE_HINT)
        .clicked()
    {
        pick = Some(Pick::Remove(person.id));
        ui.close();
    }
    pick
}

/// Applies `pick` to the call, with the notice the People panel's Remove shows.
pub fn apply(view: &mut crate::screens::call::CallView, pick: Pick) {
    match pick {
        Pick::MuteForEveryone(id) => view.call.mute_for_everyone(id),
        Pick::Remove(id) => {
            view.call.remove(id);
            view.notify("Removed. A new key epoch started, so they cannot decrypt anything new.");
        }
        Pick::Moderator(id, on) => view.call.set_moderator(id, on),
    }
}
