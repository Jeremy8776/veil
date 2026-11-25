//! The Chat side panel: messages, the reactions on them, and the message box with its
//! emoji button.
//!
//! Invariants:
//! - In a demo call, chat says it is not real.
//! - In a real call, chat and reactions are sealed on this device and sent to everyone.
//! - Only messages with an id (from 0.1.12 on) offer reactions; clicking one opens the quick
//!   reactions, and clicking a reaction under it adds or takes back yours.
//! - Pop-ups here open nothing of their own (see `call_menus`).

use std::time::Instant;

use eframe::egui::{self, Align, Button, Key, Layout, RichText, ScrollArea, TextEdit};

use crate::chat::{Message, PICKER, QUICK_REACTIONS, Reaction, Reactor};
use crate::icons::Icon;
use crate::model::clock;
use crate::screens::call::CallView;
use crate::screens::call_menus;
use crate::theme::{self, MUTED, RAISED};
use crate::widgets;

/// Emoji per row in the picker.
const PICKER_COLUMNS: usize = 8;

/// The phone gets iMessage-style bubbles, a centred empty state and a rounded input.
const PHONE: bool = cfg!(target_os = "android");

pub fn show(view: &mut CallView, ui: &mut egui::Ui, now: Instant) {
    if view.call.demo {
        ui.label(
            RichText::new("Demo: messages stay here.")
                .small()
                .color(theme::WARN),
        );
    } else if !PHONE {
        // The phone's call bar already says Encrypted.
        ui.label(RichText::new("Encrypted").small().color(MUTED))
            .on_hover_text(
                "Sent to everyone in the call, encrypted on your device. \
             Relays and forwarders cannot read it. Not kept after the call.",
            );
    }
    let input_h = if cfg!(target_os = "android") { 48.0 } else { 44.0 };
    let list_h = (ui.available_height() - input_h - 8.0).max(80.0);
    let mut react = None;
    ScrollArea::vertical()
        .max_height(list_h)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if view.call.chat.is_empty() {
                if PHONE {
                    ui.add_space(list_h * 0.4);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No messages yet").size(15.0).color(MUTED))
                    });
                } else {
                    ui.label(RichText::new("No messages yet.").color(MUTED));
                }
            }
            for message in &view.call.chat {
                if let Some(chosen) = message_row(ui, message) {
                    react = Some(chosen);
                }
            }
        });
    if let Some((id, emoji)) = react {
        view.call.react(id, &emoji);
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let input_width = (ui.available_width() - if cfg!(target_os = "android") { 112.0 } else { 76.0 }).max(1.0);
        if PHONE {
            // A rounded pill behind a frameless field, like the iMessage input.
            let pill = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(input_width, 48.0));
            ui.painter().rect_filled(pill, 24.0, theme::SURFACE);
        }
        let mut edit = TextEdit::singleline(&mut view.draft)
            .hint_text("Type a message")
            .font(theme::chat_font(ui))
            .align(egui::Align2::LEFT_CENTER)
            .char_limit(veil_call::app_msg::MAX_CHAT_CHARS)
            .desired_width(input_width);
        if PHONE {
            edit = edit
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(18, 0));
        }
        let field = ui.add_sized(
            [input_width, if cfg!(target_os = "android") { 48.0 } else { 24.0 }],
            edit,
        );
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let emoji = widgets::icon_button(ui, Icon::Emoji, "Emoji");
        if let Some(picked) = picker(&emoji) {
            if view.draft.chars().count() < veil_call::app_msg::MAX_CHAT_CHARS {
                view.draft.push_str(picked);
            }
            field.request_focus();
        }
        if widgets::icon_button(ui, Icon::Send, "Send message").clicked() || entered {
            view.call.send(&view.draft, now);
            view.draft.clear();
            field.request_focus();
        }
    });
}

/// One message with its reactions. Returns the reaction you chose, if any.
fn message_row(ui: &mut egui::Ui, message: &Message) -> Option<(u64, String)> {
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
        // The phone uses solid, rounder bubbles in the iMessage manner.
        let fill = match (message.mine, PHONE) {
            (true, true) => crate::mobile::widgets::PRIMARY,
            (true, false) => theme::ACCENT.gamma_multiply(0.35),
            (false, _) => RAISED,
        };
        let max = if PHONE { ui.available_width() * 0.75 } else { 250.0 };
        let bubble = egui::Frame::new()
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(if PHONE { 18 } else { 8 }))
            .inner_margin(if PHONE {
                egui::Margin::symmetric(14, 9)
            } else {
                egui::Margin::symmetric(10, 6)
            })
            .show(ui, |ui| {
                ui.set_max_width(max);
                let text = RichText::new(&message.text).family(theme::chat_family());
                ui.label(if PHONE { text.size(16.0).color(egui::Color32::WHITE) } else { text });
            })
            .response;
        let id = message.id?;
        let bubble = ui
            .interact(bubble.rect, bubble.id.with("react"), egui::Sense::click())
            .on_hover_text("Click to react");
        let mut chosen = quick_reactions(&bubble).map(|emoji| (id, emoji.to_string()));
        if !message.reactions.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.set_max_width(max);
                ui.spacing_mut().item_spacing.x = 4.0;
                for reaction in &message.reactions {
                    if chip(ui, reaction).clicked() {
                        chosen = Some((id, reaction.emoji.clone()));
                    }
                }
            });
        }
        chosen
    })
    .inner
}

/// The quick reactions, opened by clicking a message.
fn quick_reactions(bubble: &egui::Response) -> Option<&'static str> {
    let mut chosen = None;
    egui::Popup::from_toggle_button_response(bubble)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .align(egui::RectAlign::TOP)
        .gap(4.0)
        .frame(call_menus::frame())
        .show(|ui| {
            ui.horizontal(|ui| {
                for emoji in QUICK_REACTIONS {
                    if emoji_button(ui, emoji, 20.0).clicked() {
                        chosen = Some(emoji);
                    }
                }
            });
        });
    chosen
}

/// The emoji picker, opened by the emoji button. Stays open for several picks.
fn picker(button: &egui::Response) -> Option<&'static str> {
    let mut chosen = None;
    egui::Popup::from_toggle_button_response(button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_END)
        .gap(4.0)
        .frame(call_menus::frame())
        .show(|ui| {
            egui::Grid::new("emoji_picker")
                .spacing([2.0, 2.0])
                .show(ui, |ui| {
                    for (i, emoji) in PICKER.iter().enumerate() {
                        if emoji_button(ui, emoji, 18.0).clicked() {
                            chosen = Some(*emoji);
                        }
                        if (i + 1) % if cfg!(target_os = "android") { 6 } else { PICKER_COLUMNS } == 0 {
                            ui.end_row();
                        }
                    }
                });
        });
    chosen
}

fn emoji_button(ui: &mut egui::Ui, emoji: &str, size: f32) -> egui::Response {
    let text = RichText::new(emoji).size(size).family(theme::chat_family());
    let button = Button::new(text).frame(false);
    if cfg!(target_os = "android") { ui.add_sized([48.0, 48.0], button) } else { ui.add(button) }
}

/// A reaction under a message: the emoji and how many chose it, highlighted if you did,
/// with who chose it on hover.
fn chip(ui: &mut egui::Ui, reaction: &Reaction) -> egui::Response {
    let text = RichText::new(format!("{} {}", reaction.emoji, reaction.by.len()))
        .small()
        .family(theme::chat_family());
    let names: Vec<&str> = reaction
        .by
        .iter()
        .map(|(who, name)| {
            if *who == Reactor::You {
                "You"
            } else {
                name.as_str()
            }
        })
        .collect();
    let tip = names.join(", ");
    ui.add(
        Button::new(text)
            .selected(reaction.includes_you())
            .corner_radius(egui::CornerRadius::same(10)),
    )
    .on_hover_text(tip)
}
