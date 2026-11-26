//! The host's Lobby switch, and the warning before it goes off.
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
use crate::screens::dialog::{self, Choice};
use crate::widgets::{Tone, toggle_row};

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
    toggle_row(ui, &mut wanted, "Lobby", "").on_hover_text(
        "On: guests wait until you admit them. Off: anyone with a valid invite link joins \
         straight away. Each link still works once, and you can still remove people.",
    );
    match flip(view.call.lobby_on, wanted) {
        Flip::Apply(on) => {
            view.call.set_lobby(on);
            view.notify("Lobby on. Guests wait for you to admit them.");
        }
        Flip::Ask => view.lobby_warning = true,
        Flip::Nothing => {}
    }
}

/// The warning before the lobby goes off, while the host has it open.
pub fn show(view: &mut CallView, ctx: &egui::Context) {
    if !view.lobby_warning {
        return;
    }
    if !view.call.is_host || !view.call.lobby_on {
        view.lobby_warning = false;
        return;
    }
    let width = dialog::width(ctx.content_rect().width(), 360.0);
    let mut confirmed = false;
    let mut cancelled = false;
    let open = dialog::show(ctx, "lobby-off", "Turn off the lobby?", width, |ui| {
        ui.label("Guests with an invite link will join without waiting.")
            .on_hover_text(
                "Anyone holding a valid link can join, so share links only with people you \
                 trust. Each link still works once, the call still has a size limit, and you \
                 can still remove people. Anyone already waiting still needs you to admit them.",
            );
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
    if confirmed {
        view.call.set_lobby(false);
        view.notify("Lobby off. Guests with an invite link join straight away.");
    }
    if confirmed || cancelled || !open {
        view.lobby_warning = false;
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
