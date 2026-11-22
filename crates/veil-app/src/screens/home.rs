//! The first screen (your name, start a call, join one with an invite link, or try the demo)
//! and the screen after a call ends.
//!
//! Invariants:
//! - An invite is checked here (signature, expiry) before any device or network starts, so a
//!   bad link is reported where it was pasted. A short link is checked for its form only: the
//!   invite it stands for is fetched and verified when the call starts.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align, Layout, RichText, TextEdit};
use veil_security::short_invite::InviteLink;

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::model::clock;
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, card, pill};

const COLUMN: f32 = 440.0;

pub fn show(app: &mut VeilApp, ui: &mut egui::Ui) {
    page(ui, "home", |ui| {
        crate::logo::show(ui, 104.0, theme::PAGE);
        ui.add_space(12.0);
        ui.label(RichText::new("Private video calls, encrypted end to end.").color(MUTED));
        ui.add_space(24.0);
        update_banner(app, ui);
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
                    .hint_text("veil:…")
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
                    Err(mut problem) => {
                        // Links change form between versions; an old Veil cannot read new ones.
                        if matches!(app.updates.status(), crate::update::Status::Available(_)) {
                            problem.push_str(
                                " It may be from a newer Veil: update above, then try again.",
                            );
                        }
                        app.home_error = Some(problem);
                    }
                }
            }
            if let Some(error) = &app.home_error {
                ui.label(RichText::new(error).color(theme::DANGER_HOVER));
            }
            let keys = app.keys.clone();
            let verified = move |key: &veil_security::PublicId| {
                keys.lock().is_ok_and(|keys| keys.is_verified(key))
            };
            if let Some(invite) = app.nearby.ui(ui, verified) {
                app.joining = Some(invite);
                app.demo = false;
                start(app, false);
            }
        });
        ui.add_space(16.0);
        crate::screens::home_tools::show(app, ui, COLUMN);
    });
}

/// A small text button with no frame.
fn link(ui: &mut egui::Ui, text: &str, colour: egui::Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).small().color(colour)).frame(false))
}

/// A card above everything else while a newer Veil is out, so it is not missed: older
/// versions may not open newer invite links. Install where this build can install itself,
/// otherwise a button to the release page.
fn update_banner(app: &mut VeilApp, ui: &mut egui::Ui) {
    use crate::update::{self, Install, Status};
    let Status::Available(version) = app.updates.status() else {
        return;
    };
    let install = app.updates.install_state();
    let width = COLUMN - 40.0;
    card()
        .stroke(egui::Stroke::new(1.5, theme::ACCENT))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.label(RichText::new(format!("Veil {version} is out")).font(theme::bold(16.0)));
            ui.label(
                RichText::new(format!(
                    "You have {}. Update so you can open the newest invite links.",
                    update::VERSION
                ))
                .small()
                .color(MUTED),
            );
            ui.add_space(8.0);
            if !app.updates.can_install() {
                let button = pill(ui, None, "Get the update", Tone::Primary, width)
                    .on_hover_text("Opens the release page.");
                if button.clicked() {
                    update::open(version);
                }
                return;
            }
            match &install {
                Install::Idle | Install::Failed(_) => {
                    let button = pill(ui, None, "Install and restart", Tone::Primary, width)
                        .on_hover_text(
                            "Downloads it from GitHub, checks Veil's release signature, then \
                             installs it. Veil restarts to finish.",
                        );
                    if button.clicked() {
                        app.updates.install(version, ui.ctx());
                    }
                }
                Install::Working(step) => {
                    ui.label(RichText::new(step.as_str()).color(MUTED));
                }
                Install::Started => {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            if let Install::Failed(why) = &install {
                ui.label(
                    RichText::new(format!("Update failed: {why}"))
                        .small()
                        .color(theme::WARN),
                );
            }
            if link(ui, "What's new", MUTED)
                .on_hover_text("Opens the release page.")
                .clicked()
            {
                update::open(version);
            }
        });
    ui.add_space(12.0);
}

/// Reads an invite link, or says plainly what is wrong with it.
fn check_invite(text: &str) -> Result<InviteLink, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(String::from("Paste the invite link you were sent."));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    InviteLink::parse(text, now).map_err(|e| format!("This invite link does not work: {e}"))
}

pub(crate) fn start(app: &mut VeilApp, host: bool) {
    if app.name.trim().is_empty() {
        app.home_error = Some(String::from("Enter your name first."));
        return;
    }
    app.home_error = None;
    app.nearby.stop();
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
    page(ui, "ended", |ui| {
        ui.label(RichText::new(reason).font(theme::bold(28.0)));
        ui.label(RichText::new(format!("{title} · {}", clock(lasted))).color(MUTED));
        ui.add_space(20.0);
        if pill(ui, None, "Back to home", Tone::Primary, 220.0).clicked() {
            app.go_home();
        }
    });
}

/// A column on the page background, centred both ways. `id` names the screen: the column's
/// height is remembered from the last frame, so a screen settles one frame after it first
/// shows or changes height.
pub fn page(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE))
        .show(ui, |ui| {
            let key = ui.make_persistent_id(("page", id));
            let last: f32 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or(0.0);
            ui.add_space(top_space(ui.available_height(), last));
            let height = ui
                .with_layout(Layout::top_down(Align::Center), |ui| {
                    ui.set_max_width(COLUMN);
                    add(ui);
                })
                .response
                .rect
                .height();
            if (height - last).abs() > 0.5 {
                ui.ctx().data_mut(|d| d.insert_temp(key, height));
                ui.ctx().request_repaint();
            }
        });
}

/// Space above a column `content` high on a page `available` high: a little above centre,
/// which reads as centred, and never less than a small margin.
fn top_space(available: f32, content: f32) -> f32 {
    ((available - content) * 0.45).max(24.0)
}

#[cfg(test)]
mod tests {
    use super::top_space;

    #[test]
    fn a_short_column_sits_just_above_centre() {
        // 800 high page, 400 high column: 200 each side if centred, a little less above.
        assert_eq!(top_space(800.0, 400.0), 180.0);
    }

    #[test]
    fn a_tall_column_keeps_a_small_margin() {
        assert_eq!(top_space(500.0, 900.0), 24.0);
    }
}
