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
use crate::widgets::{Tone, card, centred_row, pill};

const COLUMN: f32 = 440.0;

pub fn show(app: &mut VeilApp, ui: &mut egui::Ui) {
    page(ui, "home", |ui| {
        crate::logo::show(ui, 104.0, theme::PAGE);
        ui.add_space(12.0);
        ui.label(RichText::new("Private video calls, encrypted end to end.").color(MUTED));
        ui.add_space(24.0);
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
        ui.add_space(20.0);
        links(app, ui);
        ui.add_space(2.0);
        updates(app, ui);
    });
}

/// One quiet row of the less-used things: the demo, servers, device key and logs.
fn links(app: &mut VeilApp, ui: &mut egui::Ui) {
    let problem = centred_row(ui, "links", |ui| {
        let demo = link(ui, "Demo call", MUTED)
            .on_hover_text("Try the interface with simulated people. Nothing is sent.");
        if demo.clicked() {
            app.demo = true;
            start(app, true);
        }
        dot(ui);
        let connection = link(ui, "Connection", MUTED)
            .on_hover_text("Your own servers and a fixed port, for calls across the internet.");
        if connection.clicked() {
            let form = crate::screens::connection::Form::new(&app.connection);
            app.screen = crate::app::Screen::Connection(Box::new(form));
        }
        dot(ui);
        let problem = device_key(app, ui);
        dot(ui);
        let logs = link(ui, "Logs", MUTED).on_hover_text(
            "Opens the folder with Veil's log files. If something goes wrong, send veil.log, or \
             veil.previous.log if Veil froze or closed. They hold states, timings and errors: \
             no names, chat or invite links.",
        );
        if logs.clicked() {
            crate::log::open_folder();
        }
        problem
    });
    if let Some(problem) = problem {
        ui.label(RichText::new(problem).small().color(theme::WARN));
    }
}

/// A small text button with no frame.
fn link(ui: &mut egui::Ui, text: &str, colour: egui::Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).small().color(colour)).frame(false))
}

fn dot(ui: &mut egui::Ui) {
    ui.label(RichText::new("·").small().color(MUTED));
}

/// This build's version, a link when a newer one is out, and the check's off switch.
fn updates(app: &mut VeilApp, ui: &mut egui::Ui) {
    use crate::update::{self, Install, Status};
    let status = app.updates.status();
    if let Status::Available(version) = status {
        let install = app.updates.install_state();
        centred_row(ui, "update", |ui| {
            let link = link(ui, &format!("Update available: {version}"), theme::WARN)
                .on_hover_text("Opens the release page.");
            if link.clicked() {
                update::open(version);
            }
            if !app.updates.can_install() {
                return;
            }
            match &install {
                Install::Idle | Install::Failed(_) => {
                    let button = ui.small_button("Install").on_hover_text(
                        "Downloads it from GitHub, checks Veil's release signature, then \
                             installs it. Veil closes to finish.",
                    );
                    if button.clicked() {
                        app.updates.install(version, ui.ctx());
                    }
                }
                Install::Working(step) => {
                    ui.label(RichText::new(step.as_str()).small().color(MUTED));
                }
                Install::Started => {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        });
        if let Install::Failed(why) = install {
            ui.label(
                RichText::new(format!("Update failed: {why}"))
                    .small()
                    .color(theme::WARN),
            );
        }
    }
    let mut on = app.updates.enabled();
    let mut text = format!("Veil {}", update::VERSION);
    if status == Status::UpToDate {
        text.push_str(" · up to date");
    }
    centred_row(ui, "version", |ui| {
        ui.label(RichText::new(text).small().color(MUTED));
        dot(ui);
        let toggle = ui
            .checkbox(
                &mut on,
                RichText::new("Check for updates").small().color(MUTED),
            )
            .on_hover_text(
                "Asks GitHub once per start whether a newer Veil is out. GitHub sees your IP \
                 address. Nothing is downloaded unless you choose Install.",
            );
        if toggle.changed() {
            app.updates.set_enabled(on, ui.ctx());
        }
    });
}

/// Your device key's short form and a way to replace it. Returns any trouble saving it.
fn device_key(app: &mut VeilApp, ui: &mut egui::Ui) -> Option<String> {
    let (short, problem) = {
        let keys = crate::media::lock(&app.keys);
        (crate::engine::short(&keys.public()), keys.problem.clone())
    };
    let label = if app.confirm_new_key {
        "Confirm new device key"
    } else {
        "New device key"
    };
    let colour = if app.confirm_new_key {
        theme::WARN
    } else {
        MUTED
    };
    let button = link(ui, label, colour).on_hover_text(format!(
        "This device is {short}. A new key means people who verified you see a new \
             device. Use it if you think your key was copied."
    ));
    if button.clicked() {
        if app.confirm_new_key {
            crate::media::lock(&app.keys).renew();
            app.confirm_new_key = false;
        } else {
            app.confirm_new_key = true;
        }
    }
    problem
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
/// height is remembered from the last frame (as `widgets::centred_row` does for width), so a
/// screen settles one frame after it first shows or changes height.
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
