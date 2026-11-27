//! A persistent call's handover on the desktop (ADR 0010 §5, §7, §8 and §9): the host's leave
//! dialog, the member's Available to host switch, the relay question and the one-line notices.
//! The phone keeps its plain Leave, which counts as Automatic.
//!
//! Invariants:
//! - Automatic (best connection) is the default pick, and the list shows names and a
//!   Good/OK/Poor bar only.
//! - Available to host is off by default and disabled while Hide my address is on.
//! - Admitting someone removed earlier in this call takes a second click.

use eframe::egui::{self, RichText};

use crate::model::handover::Stage;
use crate::screens::call::CallView;
use crate::screens::call_menus::switch_row;
use crate::screens::dialog::{self, Choice};
use crate::theme::{self, MUTED};
use crate::widgets::Tone;

/// The leave dialog's state while it is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeaveDialog {
    /// The person picked, or `None` for Automatic.
    pub pick: Option<u32>,
}

pub const AVAILABLE_TIP: &str = "If the host leaves, you may take over. Your address goes into \
    new invite links, and Veil may open a port on your router if you allow that in Settings.";
pub const HIDDEN_TIP: &str = "Turn off Hide my address to host.";
pub const PERSISTENT_TIP: &str = "The call goes on when you leave. Someone who said they are \
    available takes over hosting. Earlier invite links stop working when the host changes.";
const AUTOMATIC_TIP: &str = "Veil picks the available person with the best connection.";
const NOBODY: &str = "No one can take over. Leaving ends the call for everyone.";
const FAILED: &str = "Could not hand over. End the call for everyone, or stay?";
const LINKS: &str = "Earlier invite links no longer work.";

/// Leave was clicked. Returns whether to stay in the call: a persistent call's host gets
/// the dialog first.
pub fn leave_clicked(view: &mut CallView) -> bool {
    if view.call.asks_before_leaving() {
        view.leave_dialog = Some(LeaveDialog::default());
        return true;
    }
    false
}

/// The member's switch, for the People panel.
pub fn row(view: &mut CallView, ui: &mut egui::Ui) {
    let handover = &view.call.handover;
    if view.call.is_host || view.call.demo || !handover.persistent {
        return;
    }
    let hidden = handover.hide_address;
    let mut on = handover.available && !hidden;
    let before = on;
    ui.add_enabled_ui(!hidden, |ui| {
        switch_row(ui, &mut on, "Available to host")
            .on_hover_text(AVAILABLE_TIP)
            .on_disabled_hover_text(HIDDEN_TIP);
    });
    if on != before {
        view.call.set_available(on);
    }
}

/// Shows whatever handover dialog or notice is due this frame.
pub fn show(view: &mut CallView, ctx: &egui::Context) {
    if let Some(notice) = view.call.handover.notice.take() {
        view.notify(&notice);
    }
    relay_question(view, ctx);
    if view.leave_dialog.is_some() && !view.call.is_host {
        view.leave_dialog = None;
    }
    if let Some(open) = view.leave_dialog {
        view.leave_dialog = leave_dialog(view, ctx, open);
    }
}

fn leave_dialog(view: &mut CallView, ctx: &egui::Context, mut state: LeaveDialog) -> Option<LeaveDialog> {
    let width = dialog::width(ctx.content_rect().width(), 380.0);
    let handover = view.call.handover.clone();
    let mut close = false;
    let mut chosen: Option<Option<u32>> = None;
    let mut end = false;
    let open = dialog::show(ctx, "hand-over", "Hand over hosting", width, |ui| {
        if let Stage::Handing(name) = &handover.stage {
            let text = if name.is_empty() {
                "Handing over…".to_string()
            } else {
                format!("Handing over to {name}…")
            };
            ui.label(text).on_hover_text(LINKS);
            return;
        }
        if handover.stage == Stage::Failed {
            ui.label(FAILED);
            let (stay, stop) = dialog::pair(
                ui,
                width,
                Choice::new(None, "Stay", Tone::Quiet).hint("You stay the host."),
                Choice::new(None, "End call", Tone::Danger).hint("Ends the call for everyone."),
            );
            close = stay.clicked();
            end = stop.clicked();
            return;
        }
        if handover.choices.is_empty() {
            ui.label(NOBODY);
            let (cancel, stop) = dialog::pair(
                ui,
                width,
                Choice::new(None, "Cancel", Tone::Quiet),
                Choice::new(None, "End call", Tone::Danger).hint("Ends the call for everyone."),
            );
            close = cancel.clicked();
            end = stop.clicked();
            return;
        }
        ui.radio_value(&mut state.pick, None, "Automatic (best connection)")
            .on_hover_text(AUTOMATIC_TIP);
        for person in &handover.choices {
            let label = format!("{}  ·  {}", person.name, person.bar.label());
            ui.radio_value(&mut state.pick, Some(person.id), label)
                .on_hover_text("Connection, from their own report.");
        }
        ui.label(RichText::new(LINKS).small().color(MUTED));
        let (cancel, stop) = dialog::pair(
            ui,
            width,
            Choice::new(None, "Cancel", Tone::Quiet),
            Choice::new(None, "End call", Tone::Danger).hint("Ends the call for everyone."),
        );
        close = cancel.clicked();
        end = stop.clicked();
        if dialog::one(
            ui,
            Choice::new(None, "Hand over and leave", Tone::Primary)
                .hint("The call goes on with the new host."),
            width,
        )
        .clicked()
        {
            chosen = Some(state.pick);
        }
    });
    if end {
        view.call.end_for_everyone();
        return None;
    }
    if let Some(pick) = chosen {
        view.call.hand_over(pick);
        // Kept open: it shows "Handing over to Ana…" until the call ends here.
        return Some(state);
    }
    // While handing over, closing the dialog does not stop it.
    if close || !open {
        return matches!(handover.stage, Stage::Handing(_)).then_some(state);
    }
    Some(state)
}

/// Relay-only member: the new host's relay is another machine. Stay or leave.
fn relay_question(view: &mut CallView, ctx: &egui::Context) {
    let Some(name) = view.call.handover.relay_ask.clone() else {
        return;
    };
    let width = dialog::width(ctx.content_rect().width(), 380.0);
    let mut answer = None;
    let open = dialog::show(ctx, "relay-change", "Your relay changes", width, |ui| {
        ui.label(format!(
            "Your relay changes to {name}'s computer. {name} will be able to see your IP \
             address. Stay in the call?"
        ));
        let (leave, stay) = dialog::pair(
            ui,
            width,
            Choice::new(None, "Leave", Tone::Quiet),
            Choice::new(None, "Stay", Tone::Primary),
        );
        if leave.clicked() {
            answer = Some(false);
        }
        if stay.clicked() {
            answer = Some(true);
        }
    });
    if !open && answer.is_none() {
        answer = Some(false);
    }
    if let Some(stay) = answer {
        view.call.answer_relay(stay);
    }
}

/// The lobby tag for someone removed or declined earlier in this call.
pub fn removed_tag(ui: &mut egui::Ui) {
    ui.label(RichText::new("Removed earlier").small().color(theme::WARN))
        .on_hover_text("Removed or declined earlier in this call. Admit asks you to confirm.");
}

/// Admit was clicked for `id`. Someone removed earlier needs a second click.
pub fn admit_clicked(view: &mut CallView, id: u32) -> bool {
    if !view.call.handover.removed_earlier.contains(&id) || view.confirm_admit == Some(id) {
        view.confirm_admit = None;
        return true;
    }
    view.confirm_admit = Some(id);
    view.notify("Removed earlier in this call. Click Admit again to let them in.");
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Call;
    use std::time::Instant;

    fn view(host: bool) -> CallView {
        CallView::new(Call::live("t".into(), "Ana", host, Instant::now()), None)
    }

    #[test]
    fn a_plain_call_leaves_at_once_and_a_persistent_host_is_asked() {
        let mut plain = view(true);
        assert!(!leave_clicked(&mut plain));
        let mut persistent = view(true);
        persistent.call.handover.persistent = true;
        assert!(leave_clicked(&mut persistent));
        assert_eq!(persistent.leave_dialog, Some(LeaveDialog { pick: None }));
    }

    #[test]
    fn someone_removed_earlier_needs_a_second_admit() {
        let mut host = view(true);
        host.call.handover.removed_earlier = vec![4];
        assert!(!admit_clicked(&mut host, 4));
        assert!(admit_clicked(&mut host, 4));
        assert!(admit_clicked(&mut host, 5), "others are admitted at once");
    }
}
