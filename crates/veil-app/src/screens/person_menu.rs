//! The right-click menu on a person: on their tile and on their row in People.
//!
//! Purpose: everyone gets that person's volume and "Mute for me", which change only what this
//! device plays (`volume.rs`). The host and moderators also get "Mute for everyone" and
//! "Remove from call"; the host also names or drops moderators (`model/moderation.rs`).
//!
//! Invariants:
//! - Built from `call_menus` pieces, so it looks like the bar's menus. It opens no pop-up of
//!   its own inside.
//! - Every row shares one icon column and one text edge (`call_menus::ROW_ICON_X`,
//!   `ROW_TEXT_X`): what is for you first, then the host's Moderator switch, then
//!   "Moderator actions" (Mute for everyone and Remove from call), both in red.
//! - Your own tile and row have no menu. Nobody gets moderation controls on the host.
//! - The menu only returns a [`Pick`]; the caller applies it to the call after drawing, so a
//!   person list being drawn is never changed under it.

use eframe::egui::{
    self, Align2, CornerRadius, FontId, PopupCloseBehavior, Rect, Sense, pos2, vec2,
};

use crate::icons::Icon;
use crate::model::{Call, Participant};
use crate::screens::call_menus;
use crate::screens::stats::short_name;
use crate::theme;
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
    let role = if call.is_moderator(person.id) {
        Some("Moderator")
    } else if person.is_host {
        Some("Host")
    } else {
        None
    };
    header(ui, &short_name(&person.name), role);
    let mut level = playback.voice_level(person.id);
    if volume(ui, &mut level.volume) {
        playback.set_voice_level(person.id, level);
    }
    if call_menus::icon_switch_row(ui, Icon::MicOff, &mut level.muted, "Mute for me")
        .on_hover_text(MUTE_ME_HINT)
        .changed()
    {
        playback.set_voice_level(person.id, level);
    }
    if !call.can_moderate() || person.is_host {
        return None;
    }
    let mut pick = None;
    if call.is_host {
        call_menus::divider(ui);
        let mut moderator = call.is_moderator(person.id);
        if call_menus::icon_switch_row(ui, Icon::Shield, &mut moderator, "Moderator")
            .on_hover_text(MODERATOR_HINT)
            .changed()
        {
            pick = Some(Pick::Moderator(person.id, moderator));
        }
    }
    call_menus::divider(ui);
    heading(ui, "Moderator actions");
    if !person.muted
        && action(ui, Icon::MicOff, "Mute for everyone", true)
            .on_hover_text(MUTE_ALL_HINT)
            .clicked()
    {
        pick = Some(Pick::MuteForEveryone(person.id));
        ui.close();
    }
    if action(ui, Icon::Leave, "Remove from call", true)
        .on_hover_text(REMOVE_HINT)
        .clicked()
    {
        pick = Some(Pick::Remove(person.id));
        ui.close();
    }
    pick
}

/// The person's name in bold, and their role under it when they have one.
fn header(ui: &mut egui::Ui, name: &str, role: Option<&str>) {
    let height = if role.is_some() { 44.0 } else { 32.0 };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    let left = rect.left() + 8.0;
    let name_y = if role.is_some() {
        rect.top() + 14.0
    } else {
        rect.center().y
    };
    painter.text(
        pos2(left, name_y),
        Align2::LEFT_CENTER,
        name,
        theme::bold(14.0),
        theme::TEXT,
    );
    if let Some(role) = role {
        painter.text(
            pos2(left, rect.top() + 32.0),
            Align2::LEFT_CENTER,
            role,
            FontId::proportional(12.0),
            theme::MUTED,
        );
    }
}

/// A small muted heading over a group of rows, on the icon column's left edge.
fn heading(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::bold(11.5),
        theme::MUTED,
    );
}

/// The volume row on the menu's icon column: icon, slider, then the percentage.
fn volume(ui: &mut egui::Ui, gain: &mut f32) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let at = pos2(rect.left() + call_menus::ROW_ICON_X, rect.center().y);
    Icon::Volume.paint(ui.painter(), at, 15.0, theme::MUTED);
    let percent = Rect::from_min_max(pos2(rect.right() - 44.0, rect.top()), rect.max);
    let track = Rect::from_min_max(
        pos2(rect.left() + call_menus::ROW_TEXT_X - 7.0, rect.top()),
        pos2(percent.left() - 4.0, rect.bottom()),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(track)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let changed = call_menus::slider(&mut child, gain, VOICE_MAX, track.width(), "Volume")
        .on_hover_text(VOLUME_HINT)
        .changed();
    ui.painter().text(
        pos2(percent.right() - 8.0, percent.center().y),
        Align2::RIGHT_CENTER,
        format!("{}%", call_menus::percent(*gain)),
        FontId::proportional(12.5),
        theme::MUTED,
    );
    changed
}

/// A menu action on the icon column. A `danger` action (every moderator action) is red.
fn action(ui: &mut egui::Ui, icon: Icon, label: &str, danger: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let hot = response.hovered() || response.has_focus();
    if hot {
        painter.rect_filled(rect, CornerRadius::same(6), theme::RAISED_HOVER);
    }
    let (icon_colour, text_colour) = match (danger, hot) {
        (true, true) => (theme::DANGER_HOVER, theme::DANGER_HOVER),
        (true, false) => (theme::DANGER, theme::DANGER),
        (false, _) => (theme::MUTED, theme::TEXT),
    };
    let at = pos2(rect.left() + call_menus::ROW_ICON_X, rect.center().y);
    icon.paint(painter, at, 15.0, icon_colour);
    painter.text(
        pos2(rect.left() + call_menus::ROW_TEXT_X, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.5),
        text_colour,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
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
