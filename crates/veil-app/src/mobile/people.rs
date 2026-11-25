//! The phone's People page, as an Apple-style grouped list: an invite group (host), the
//! lobby with Admit and Decline (host), and everyone in the call. Short labels; detail such
//! as safety numbers and connection state lives in tooltips.
//!
//! Invariants (the same as the desktop panel in `screens/panels.rs`):
//! - Only the host sees Admit, Decline and Remove.
//! - A person in the lobby shows their safety number on hover, so the host can compare it.
//! - "Verified" is only a label for a device compared before. The host still admits.
use super::widgets as w;
use crate::icons::Icon;
use crate::model::{Action, Participant, clock};
use crate::screens::{call::CallView, invite, panels};
use crate::theme;
use crate::widgets::{Tone, avatar};
use eframe::egui::{self, Align, Layout, Sense, vec2};
use std::time::Instant;

/// Avatar size in a row, and the row height.
const AVATAR: f32 = 40.0;
const ROW: f32 = 60.0;

pub(super) fn show(view: &mut CallView, ui: &mut egui::Ui, now: Instant) {
    if view.call.is_host && !view.call.demo {
        header(ui, "Invite");
        group(ui, |ui| {
            if w::menu_item(ui, Some(Icon::People), "Add people", false)
                .on_hover_text(invite::lifetime_text(&view.call, invite::now_unix()))
                .clicked()
            {
                view.mobile_invite = true;
            }
            let mut until_end = view.call.invite_until_end;
            if super::setting(
                ui,
                &mut until_end,
                "Keep links working",
                "Until the call ends. Each link still lets one person in, and you still admit them.",
            )
            .changed()
            {
                view.call.invite_until_end = until_end;
                view.call.outbox.push(Action::InviteUntilEnd(until_end));
            }
        });
        panels::host_status(view, ui);
    } else if w::link(ui, Some(Icon::Copy), "Copy invite link")
        .on_hover_text("A new link is made after each use.")
        .clicked()
    {
        view.copy_invite(ui.ctx());
    }
    let mut admit = None;
    let mut decline = None;
    let mut remove = None;
    if view.call.is_host && !view.call.lobby.is_empty() {
        header(ui, &format!("Waiting ({})", view.call.lobby.len()));
        group(ui, |ui| {
            for waiting in &view.call.lobby {
                let detail = if waiting.verified {
                    "Verified"
                } else if waiting.safety.is_empty() {
                    ""
                } else {
                    "New device"
                };
                let waited = clock(now.saturating_duration_since(waiting.since));
                let sub = if detail.is_empty() { waited } else { format!("{waited} · {detail}") };
                row(ui, &waiting.name, &sub, |ui| {
                    if w::action(ui, None, "Admit", Tone::Primary, 80.0).clicked() {
                        admit = Some(waiting.id);
                    }
                    if w::action(ui, None, "Decline", Tone::Quiet, 88.0).clicked() {
                        decline = Some(waiting.id);
                    }
                })
                .on_hover_text(if waiting.safety.is_empty() {
                    String::new()
                } else {
                    format!(
                        "Safety number: {}\nCompare it with them before letting them in.",
                        waiting.safety
                    )
                });
            }
        });
    }
    header(ui, &format!("In this call ({})", view.call.participants.len()));
    group(ui, |ui| {
        for person in &view.call.participants {
            let sub = subtitle(person.is_you, person.is_host, person.verified);
            row(ui, &person.name, &sub, |ui| {
                if view.call.is_host
                    && !person.is_you
                    && w::icon(ui, Icon::Close, &format!("Remove {}", person.name), false).clicked()
                {
                    remove = Some(person.id);
                }
                mic(ui, person);
            });
        }
    });
    if let Some(id) = admit {
        view.call.admit(id);
        view.notify("Admitted");
    }
    if let Some(id) = decline {
        view.call.decline(id);
    }
    if let Some(id) = remove {
        view.call.remove(id);
        view.notify("Removed");
    }
}

/// "You", "Host" and "Verified", joined by dots. Empty for a plain guest.
fn subtitle(is_you: bool, is_host: bool, verified: bool) -> String {
    let mut parts = Vec::new();
    if is_you {
        parts.push("You");
    }
    if is_host {
        parts.push("Host");
    }
    if verified {
        parts.push("Verified");
    }
    parts.join(" · ")
}

fn header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        w::label(ui, text, 13.0, false, theme::MUTED);
    });
    ui.add_space(8.0);
}

/// A rounded group of rows on the raised surface.
fn group(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .corner_radius(16)
        .inner_margin(egui::Margin::symmetric(16, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui);
        });
}

/// One person: avatar, name, a muted subtitle, and trailing controls on the right.
fn row(ui: &mut egui::Ui, name: &str, sub: &str, trailing: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    avatar(ui.painter(), egui::pos2(rect.left() + AVATAR * 0.5, rect.center().y), AVATAR * 0.5, name);
    let text = egui::Rect::from_min_max(
        egui::pos2(rect.left() + AVATAR + 12.0, rect.top()),
        rect.max,
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text).layout(Layout::right_to_left(Align::Center)));
    child.spacing_mut().item_spacing.x = 8.0;
    trailing(&mut child);
    let left = egui::Rect::from_min_max(text.min, egui::pos2(child.min_rect().left() - 8.0, text.bottom()));
    let mut names = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(Layout::top_down(Align::Min)));
    names.spacing_mut().item_spacing.y = 0.0;
    names.add_space(((ROW - if sub.is_empty() { 20.0 } else { 38.0 }) * 0.5).max(0.0));
    w::label(&mut names, name, 16.0, false, theme::TEXT);
    if !sub.is_empty() {
        w::label(&mut names, sub, 13.0, false, theme::MUTED);
    }
    response
}

/// The microphone state: red slashed when muted, green while speaking, a raised hand beside.
fn mic(ui: &mut egui::Ui, person: &Participant) {
    let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    if person.muted {
        Icon::MicOff.paint(ui.painter(), rect.center(), 18.0, theme::DANGER_HOVER);
    } else {
        let ink = if person.speaking { theme::SPEAKING } else { theme::MUTED };
        Icon::Mic.paint(ui.painter(), rect.center(), 18.0, ink);
    }
    if person.hand_raised {
        let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
        Icon::Hand.paint(ui.painter(), rect.center(), 18.0, theme::WARN);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_joins_roles() {
        assert_eq!(subtitle(false, false, false), "");
        assert_eq!(subtitle(true, true, false), "You · Host");
        assert_eq!(subtitle(false, false, true), "Verified");
    }
}
