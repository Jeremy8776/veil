//! The first screen (your name, start a call, or join one with an invite link) and the screen
//! after a call ends.

use eframe::egui::{self, Align, Layout, RichText, TextEdit};

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
                if app.invite.trim().is_empty() {
                    app.home_error = Some(String::from("Paste the invite link you were sent."));
                } else {
                    start(app, false);
                }
            }
            if let Some(error) = &app.home_error {
                ui.label(RichText::new(error).color(theme::DANGER_HOVER));
            }
        });
        ui.add_space(16.0);
        ui.label(
            RichText::new(
                "Pictures and sound are encrypted on your device. Servers that help connect a \
                 call carry only sealed packets.",
            )
            .small()
            .color(MUTED),
        );
    });
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
    let crate::app::Screen::Ended { title, lasted } = &app.screen else {
        return;
    };
    let (title, lasted) = (title.clone(), *lasted);
    page(ui, |ui| {
        ui.label(RichText::new("You left the call").font(theme::bold(28.0)));
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
