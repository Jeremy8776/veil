//! The call's side panels: People (with the lobby), Chat (drawn by `chat_panel.rs`), and
//! Security (drawn by `security_panel.rs`). One is open at a time, on the right.
//!
//! Invariants:
//! - Only the host sees Admit, Decline and Remove.
//! - A person in the lobby shows their safety number on hover, so the host can compare it
//!   before letting them in.
//! - "Verified" is only a label for a device you compared before. The host still admits.

use std::time::Instant;

use eframe::egui::{self, Align, Layout, RichText, ScrollArea, Sense, vec2};

use crate::icons::Icon;
use crate::model::{Participant, Reachable, RouterPort, clock};
use crate::screens::call::CallView;
use crate::screens::{chat_panel, security_panel};
use crate::theme::{self, MUTED, SURFACE};
use crate::widgets::{self, Tone, pill};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    People,
    Chat,
    Security,
}

pub fn show(view: &mut CallView, ui: &mut egui::Ui, panel: Panel, now: Instant) {
    egui::Panel::right("call_panel")
        .exact_size(340.0)
        .frame(
            egui::Frame::new()
                .fill(SURFACE)
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ui, |ui| {
            let title = match panel {
                Panel::People => "People",
                Panel::Chat => "Chat",
                Panel::Security => "Security",
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).font(theme::bold(17.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, Icon::Close, "Close panel").clicked() {
                        view.panel = None;
                    }
                });
            });
            ui.add_space(4.0);
            match panel {
                Panel::People => people(view, ui, now),
                Panel::Chat => chat_panel::show(view, ui, now),
                Panel::Security => security_panel::show(view, ui),
            }
        });
}

/// For a host who asked for it: whether the router opened the call's port, with the address
/// or the guide in the tooltip.
fn router(ui: &mut egui::Ui, port: &RouterPort) {
    let (text, colour, tip) = match port {
        RouterPort::Off => return,
        RouterPort::Opening => ("Opening router ports…", MUTED, String::new()),
        RouterPort::Open(addr) => (
            "Router port open",
            MUTED,
            format!(
                "Guests on the internet can reach you at {addr}. The invite link includes it. \
                 Closed when the call ends."
            ),
        ),
        RouterPort::Failed(guide) => ("Router did not open a port", theme::WARN, guide.clone()),
        RouterPort::GuestsApart(addr) => (
            "Guests may not see each other",
            theme::WARN,
            format!(
                "Guests on the internet can reach you at {addr}, but your router did not open \
                 the second port they use to reach each other. To fix this, add a public \
                 veil-node under Settings > Connection > Rendezvous and relay."
            ),
        ),
    };
    ui.add_space(4.0);
    let label = ui.label(RichText::new(text).small().color(colour));
    if !tip.is_empty() {
        label.on_hover_text(tip);
    }
}

/// For a host with a public node: whether it could dial the call's public address, with the
/// detail in the tooltip.
fn reach(ui: &mut egui::Ui, reach: &Reachable) {
    let (text, colour, tip) = match reach {
        Reachable::Unknown => return,
        Reachable::Checking => ("Checking from outside…", MUTED, ""),
        Reachable::Yes(tip) => ("Reachable from the internet", MUTED, tip.as_str()),
        Reachable::No(tip) => ("Not reachable from the internet", theme::WARN, tip.as_str()),
    };
    let label = ui.label(RichText::new(text).small().color(colour));
    if !tip.is_empty() {
        label.on_hover_text(tip);
    }
}

fn people(view: &mut CallView, ui: &mut egui::Ui, now: Instant) {
    let copy = pill(
        ui,
        Some(Icon::Copy),
        "Copy invite link",
        Tone::Quiet,
        ui.available_width(),
    )
    .on_hover_text("A new link is made after each use.");
    if copy.clicked() {
        view.copy_invite(ui.ctx());
    }
    if view.call.is_host && !view.call.demo {
        crate::screens::invite::lifetime(view, ui);
    }
    if view.call.is_host {
        router(ui, &view.call.router);
        reach(ui, &view.call.reach);
    }
    ui.add_space(8.0);
    let mut admit = None;
    let mut decline = None;
    let mut remove = None;
    if view.call.is_host && !view.call.lobby.is_empty() {
        section(
            ui,
            &format!("Waiting in the lobby ({})", view.call.lobby.len()),
        );
        for waiting in &view.call.lobby {
            ui.horizontal(|ui| {
                person_avatar(ui, &waiting.name);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let name = ui.label(&waiting.name);
                    if !waiting.safety.is_empty() {
                        name.on_hover_text(format!(
                            "Safety number: {}
Compare it with them before letting them in.",
                            waiting.safety
                        ));
                    }
                    let waited = clock(now.saturating_duration_since(waiting.since));
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("Waiting {waited}"))
                                .small()
                                .color(MUTED),
                        );
                        if waiting.verified {
                            verified_tag(ui).on_hover_text(
                                "You verified this device in an earlier call. You still decide whether to let them in.",
                            );
                        } else if !waiting.safety.is_empty() {
                            ui.label(RichText::new("New device").small().color(theme::WARN))
                                .on_hover_text(
                                    "You have not verified this device. Compare the safety number before letting them in.",
                                );
                        }
                    });
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(40.0);
                if pill(ui, None, "Admit", Tone::Primary, 96.0).clicked() {
                    admit = Some(waiting.id);
                }
                if pill(ui, None, "Decline", Tone::Quiet, 96.0).clicked() {
                    decline = Some(waiting.id);
                }
            });
        }
        ui.add_space(8.0);
    }
    section(
        ui,
        &format!("In this call ({})", view.call.participants.len()),
    );
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for person in &view.call.participants {
                ui.horizontal(|ui| {
                    person_avatar(ui, &person.name);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let you = if person.is_you { " (You)" } else { "" };
                        ui.horizontal(|ui| {
                            ui.label(format!("{}{you}", person.name));
                            if person.verified {
                                verified_tag(ui).on_hover_text("You verified this device.");
                            }
                        });
                        if person.is_host {
                            ui.label(RichText::new("Host").small().color(MUTED));
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if view.call.is_host
                            && !person.is_you
                            && widgets::icon_button(
                                ui,
                                Icon::Close,
                                &format!("Remove {}", person.name),
                            )
                            .clicked()
                        {
                            remove = Some(person.id);
                        }
                        status_icons(ui, person);
                    });
                });
            }
        });
    if let Some(id) = admit {
        view.call.admit(id);
        view.notify("Admitted. A new key epoch started.");
    }
    if let Some(id) = decline {
        view.call.decline(id);
    }
    if let Some(id) = remove {
        view.call.remove(id);
        view.notify("Removed. A new key epoch started, so they cannot decrypt anything new.");
    }
}

fn status_icons(ui: &mut egui::Ui, person: &Participant) {
    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    if person.muted {
        Icon::MicOff.paint(ui.painter(), rect.center(), 14.0, theme::DANGER_HOVER);
    } else {
        let colour = if person.speaking {
            theme::SPEAKING
        } else {
            MUTED
        };
        Icon::Mic.paint(ui.painter(), rect.center(), 14.0, colour);
    }
    if person.hand_raised {
        let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        Icon::Hand.paint(ui.painter(), rect.center(), 14.0, theme::WARN);
    }
}

/// A small green "Verified" label with a shield.
fn verified_tag(ui: &mut egui::Ui) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
        Icon::Shield.paint(ui.painter(), rect.center(), 11.0, theme::SPEAKING);
        ui.label(RichText::new("Verified").small().color(theme::SPEAKING));
    })
    .response
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED));
}

fn person_avatar(ui: &mut egui::Ui, name: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
    widgets::avatar(ui.painter(), rect.center(), 16.0, name);
}
