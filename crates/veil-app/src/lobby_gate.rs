//! The host's Lobby switch, and the warning before it goes off. The phone draws the same
//! switch and warning as a sheet (`mobile/people.rs`) and acts through [`moved`] and [`answer`].
//!
//! Purpose: with the lobby on (the default) the host admits each guest. Turning it off lets
//! anyone with a valid invite link come straight in, so the host confirms first in a dialog
//! with Cancel and Turn off. Turning it back on needs no confirmation.
//!
//! Invariants:
//! - [`flip`] is pure. Turning the switch off only opens the warning; only Turn off in the
//!   warning changes the call.
//! - The host session enforces the switch (`veil_call::HostHandle::set_gate`). This module
//!   only asks and queues it. Identity checks, single-use invites, the seat limit and removal
//!   apply either way.
//! - With the lobby off the host gets no lobby reminder or alert: nobody new waits.

use eframe::egui;

use crate::screens::call::CallView;
use crate::screens::call_menus::switch_row;
use crate::screens::dialog::{self, Choice};
use crate::widgets::Tone;

/// What a click on the Lobby switch does.
#[derive(Debug, PartialEq, Eq)]
pub enum Flip {
    /// Change the call now.
    Apply(bool),
    /// Open the warning; the call keeps its lobby until the host confirms.
    Ask,
    /// The switch did not move.
    Nothing,
}

/// What moving the switch from `on` to `wanted` does: off asks first, on applies at once.
pub fn flip(on: bool, wanted: bool) -> Flip {
    match (on, wanted) {
        (true, false) => Flip::Ask,
        (false, true) => Flip::Apply(true),
        _ => Flip::Nothing,
    }
}

/// The host's Lobby switch, for the People panel. Not in a demo, which has no real guests.
pub fn row(view: &mut CallView, ui: &mut egui::Ui) {
    if !view.call.is_host || view.call.demo {
        return;
    }
    let mut wanted = view.call.lobby_on;
    switch_row(ui, &mut wanted, "Lobby").on_hover_text(
        "On: guests wait until you admit them. Off: anyone with a valid invite link joins \
         straight away. Each link still works once, and you can still remove people.",
    );
    moved(view, wanted);
}

/// Acts on the host moving the Lobby switch to `wanted`, on desktop or phone.
pub fn moved(view: &mut CallView, wanted: bool) {
    match flip(view.call.lobby_on, wanted) {
        Flip::Apply(on) => {
            view.call.set_lobby(on);
            view.notify("Lobby on. Guests wait for you to admit them.");
        }
        Flip::Ask => view.lobby_warning = true,
        Flip::Nothing => {}
    }
}

/// Whether the warning should be drawn this frame. Closes it once it no longer applies.
pub fn warning_open(view: &mut CallView) -> bool {
    if view.lobby_warning && (!view.call.is_host || !view.call.lobby_on) {
        view.lobby_warning = false;
    }
    view.lobby_warning
}

/// The host's answer in the warning: Turn off (`true`), or Cancel or closing it (`false`).
pub fn answer(view: &mut CallView, turn_off: bool) {
    if turn_off {
        view.call.set_lobby(false);
        view.notify("Lobby off. Guests with an invite link join straight away.");
    }
    view.lobby_warning = false;
}

/// The warning's line and its detail, shared by the desktop dialog and the phone sheet.
pub const WARNING: &str = "Guests with an invite link will join without waiting.";
pub const WARNING_DETAIL: &str = "Anyone holding a valid link can join, so share links only \
    with people you trust. Each link still works once, the call still has a size limit, and \
    you can still remove people. Anyone already waiting still needs you to admit them.";

/// The warning before the lobby goes off, while the host has it open.
pub fn show(view: &mut CallView, ctx: &egui::Context) {
    if !warning_open(view) {
        return;
    }
    let width = dialog::width(ctx.content_rect().width(), 360.0);
    let mut confirmed = false;
    let mut cancelled = false;
    let open = dialog::show(ctx, "lobby-off", "Turn off the lobby?", width, |ui| {
        ui.label(WARNING).on_hover_text(WARNING_DETAIL);
        let (cancel, off) = dialog::pair(
            ui,
            width,
            Choice::new(None, "Cancel", Tone::Quiet).hint("Keeps the lobby on."),
            Choice::new(None, "Turn off", Tone::Primary)
                .hint("Lets guests with a valid link join straight away."),
        );
        confirmed = off.clicked();
        cancelled = cancel.clicked();
    });
    if confirmed || cancelled || !open {
        answer(view, confirmed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_the_lobby_off_asks_first() {
        assert_eq!(flip(true, false), Flip::Ask);
    }

    #[test]
    fn turning_the_lobby_on_needs_no_confirmation() {
        assert_eq!(flip(false, true), Flip::Apply(true));
    }

    #[test]
    fn a_switch_that_did_not_move_does_nothing() {
        assert_eq!(flip(true, true), Flip::Nothing);
        assert_eq!(flip(false, false), Flip::Nothing);
    }
}
