//! The first screen (your name, start a call, join one with an invite link, or try the demo)
//! and the screen after a call ends.
//!
//! Invariants:
//! - An invite is checked here (signature, expiry) before any device or network starts, so a
//!   bad link is reported where it was pasted.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align, Layout, RichText, TextEdit};
use veil_security::invite::Invite;

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::model::clock;
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, card, pill};

const COLUMN: f32 = 440.0;

pub fn show(app: &mut VeilApp, ui: &mut egui::Ui) {
    page(ui, |ui| {
        ui.label(RichText::new("Veil").font(theme::bold(40.0)));
        ui.label(RichText::new("Private video calls, encrypted end to end.").color(MUTED));
        ui.add_space(20.0);
        card().show(ui, |ui| {
            ui.set_width(COLUMN - 40.0);
            ui.label(RichText::new("Your name").color(MUTED));
            ui.add(
                TextEdit::singleline(&mut app.name)
                    .hint_text("How others see you")
                    .char_limit(veil_call::app_msg::MAX_NAME_CHARS)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            if pill(
                ui,
                Some(Icon::Video),
                "New call",
                Tone::Primary,
                COLUMN - 40.0,
            )
            .clicked()
            {
                app.demo = false;
                start(app, true);
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Or join with an invite link").color(MUTED));
            let field = ui.add(
                TextEdit::singleline(&mut app.invite)
                    .hint_text("veil://join/…")
                    .desired_width(f32::INFINITY),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if pill(ui, Some(Icon::Link), "Join", Tone::Quiet, COLUMN - 40.0).clicked() || entered {
                match check_invite(&app.invite) {
                    Ok(invite) => {
                        app.joining = Some(invite);
                        app.demo = false;
                        start(app, false);
                    }
                    Err(problem) => app.home_error = Some(problem),
                }
            }
            if let Some(error) = &app.home_error {
                ui.label(RichText::new(error).color(theme::DANGER_HOVER));
            }
        });
        ui.add_space(16.0);
        ui.label(RichText::new("End-to-end encrypted").small().color(MUTED));
        ui.add_space(8.0);
        let demo = ui
            .add(egui::Button::new(RichText::new("Demo call").small().color(MUTED)).frame(false))
            .on_hover_text("Try the interface with simulated people. Nothing is sent.");
        if demo.clicked() {
            app.demo = true;
            start(app, true);
        }
        device_key(app, ui);
    });
}

/// Your device key's short form, a way to replace it, and any trouble saving it.
fn device_key(app: &mut VeilApp, ui: &mut egui::Ui) {
    let (short, problem) = {
        let keys = crate::media::lock(&app.keys);
        (crate::engine::short(&keys.public()), keys.problem.clone())
    };
    ui.add_space(4.0);
    let label = if app.confirm_new_key { "Confirm new device key" } else { "New device key" };
    let colour = if app.confirm_new_key { theme::WARN } else { MUTED };
    let button = ui
        .add(egui::Button::new(RichText::new(label).small().color(colour)).frame(false))
        .on_hover_text(format!(
            "This device is {short}. A new key means people who verified you see a new              device. Use it if you think your key was copied."
        ));
    if button.clicked() {
        if app.confirm_new_key {
            crate::media::lock(&app.keys).renew();
            app.confirm_new_key = false;
        } else {
            app.confirm_new_key = true;
        }
    }
    if let Some(problem) = problem {
        ui.label(RichText::new(problem).small().color(theme::WARN));
    }
}

/// Reads an invite link, or says plainly what is wrong with it.
fn check_invite(text: &str) -> Result<Invite, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(String::from("Paste the invite link you were sent."));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Invite::from_link(text, now).map_err(|e| format!("This invite link does not work: {e}"))
}

fn start(app: &mut VeilApp, host: bool) {
    if app.name.trim().is_empty() {
        app.home_error = Some(String::from("Enter your name first."));
        return;
    }
    app.home_error = None;
    app.go_pre_join(host);
}

/// After leaving: how long the call lasted, and the way back.
pub fn ended(app: &mut VeilApp, ui: &mut egui::Ui) {
    let crate::app::Screen::Ended {
        title,
        lasted,
        reason,
    } = &app.screen
    else {
        return;
    };
    let (title, lasted, reason) = (title.clone(), *lasted, reason.clone());
    page(ui, |ui| {
        ui.label(RichText::new(reason).font(theme::bold(28.0)));
        ui.label(RichText::new(format!("{title} · {}", clock(lasted))).color(MUTED));
        ui.add_space(20.0);
        if pill(ui, None, "Back to home", Tone::Primary, 220.0).clicked() {
            app.go_home();
        }
    });
}

/// A centred column on the page background.
fn page(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE))
        .show(ui, |ui| {
            let top = (ui.available_height() * 0.18).max(24.0);
            ui.add_space(top);
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.set_max_width(COLUMN);
                add(ui);
            });
        });
}
