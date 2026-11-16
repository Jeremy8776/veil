//! The call's side panels: People (with the lobby), Chat, and Security (safety numbers and
//! how each person is connected). One is open at a time, on the right.
//!
//! Invariants:
//! - Only the host sees Admit, Decline and Remove.
//! - In a demo call, chat and safety numbers say they are not real.
//! - In a real call, chat says it stays on this device: messages are not sent yet.
//! - A person in the lobby shows their safety number on hover, so the host can compare it
//!   before letting them in.

use std::time::Instant;

use eframe::egui::{self, Align, Key, Layout, RichText, ScrollArea, Sense, TextEdit, vec2};

use crate::icons::Icon;
use crate::model::{LinkPath, Participant, clock};
use crate::screens::call::CallView;
use crate::theme::{self, MUTED, RAISED, SURFACE};
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
                Panel::Chat => chat(view, ui, now),
                Panel::Security => security(view, ui),
            }
        });
}

fn people(view: &mut CallView, ui: &mut egui::Ui, now: Instant) {
    let copy = pill(
        ui,
        Some(Icon::Copy),
        "Copy invite link",
        Tone::Quiet,
        ui.available_width(),
    )
    .on_hover_text("Works once and for an hour. A new link is made after each use.");
    if copy.clicked() && !view.call.invite.is_empty() {
        ui.ctx().copy_text(view.call.invite.clone());
        view.notify("Invite link copied");
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
                    ui.label(
                        RichText::new(format!("Waiting {waited}"))
                            .small()
                            .color(MUTED),
                    );
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
                        ui.label(format!("{}{you}", person.name));
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

fn chat(view: &mut CallView, ui: &mut egui::Ui, now: Instant) {
    let note = if view.call.demo {
        "Demo: messages stay here."
    } else {
        "Not sent yet: messages stay on this device."
    };
    ui.label(RichText::new(note).small().color(theme::WARN));
    let input_h = 44.0;
    let list_h = (ui.available_height() - input_h - 8.0).max(80.0);
    ScrollArea::vertical()
        .max_height(list_h)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if view.call.chat.is_empty() {
                ui.label(RichText::new("No messages yet.").color(MUTED));
            }
            for message in &view.call.chat {
                let layout = if message.mine {
                    Layout::top_down(Align::Max)
                } else {
                    Layout::top_down(Align::Min)
                };
                ui.with_layout(layout, |ui| {
                    let who = if message.mine { "You" } else { &message.from };
                    ui.label(
                        RichText::new(format!("{who} · {}", clock(message.at)))
                            .small()
                            .color(MUTED),
                    );
                    let fill = if message.mine {
                        theme::ACCENT.gamma_multiply(0.35)
                    } else {
                        RAISED
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_max_width(250.0);
                            ui.label(&message.text);
                        });
                });
            }
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let field = ui.add(
            TextEdit::singleline(&mut view.draft)
                .hint_text("Type a message")
                .desired_width(ui.available_width() - 40.0),
        );
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if widgets::icon_button(ui, Icon::Send, "Send message").clicked() || entered {
            view.call.send(&view.draft, now);
            view.draft.clear();
            field.request_focus();
        }
    });
}

fn security(view: &mut CallView, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        Icon::Shield.paint(ui.painter(), rect.center(), 18.0, theme::SPEAKING);
        ui.label(RichText::new("End-to-end encrypted").color(theme::SPEAKING))
            .on_hover_text("Encrypted on each device. Relays and forwarders cannot open it.");
        let epoch = format!("Key {}", view.call.epoch);
        ui.label(RichText::new(epoch).small().color(MUTED))
            .on_hover_text("A new key starts whenever someone joins or leaves.");
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        section(ui, "Safety numbers");
        if view.call.demo {
            ui.label(RichText::new("demo").small().color(theme::WARN))
                .on_hover_text("Made up for the demo call.");
        }
        let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        Icon::Info.paint(ui.painter(), rect.center(), 12.0, MUTED);
        response.on_hover_text(
            "Compare with each person over another channel. A match means nobody is in between.",
        );
    });
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for person in view.call.participants.iter().filter(|p| !p.is_you) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    person_avatar(ui, &person.name);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(&person.name);
                        path_label(ui, person.path);
                    });
                });
                let groups: Vec<&str> = person.safety.split(' ').collect();
                for row in groups.chunks(4) {
                    ui.label(RichText::new(row.join("  ")).monospace().size(13.0));
                }
            }
        });
}

fn path_label(ui: &mut egui::Ui, path: LinkPath) {
    let colour = match path {
        LinkPath::Relayed => theme::WARN,
        LinkPath::Pending => MUTED,
        _ => theme::SPEAKING,
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
        Icon::Signal.paint(ui.painter(), rect.center(), 12.0, colour);
        ui.label(RichText::new(path.label()).small().color(colour));
    });
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED));
}

fn person_avatar(ui: &mut egui::Ui, name: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
    widgets::avatar(ui.painter(), rect.center(), 16.0, name);
}
