//! Before joining: see yourself, check the microphone moves the meter, choose whether to come
//! in muted or with the camera off.

use eframe::egui::{self, Align, Layout, RichText, Sense, vec2};

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::theme::{self, MUTED};
use crate::widgets::{self, ControlState, Tile, Tone, card, control, level_meter, pill};

pub fn show(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::PAGE)
                .inner_margin(egui::Margin::same(32)),
        )
        .show(ui, |ui| {
            let title = if host { "Start a call" } else { "Join a call" };
            ui.label(RichText::new(title).font(theme::bold(26.0)));
            ui.add_space(12.0);
            let wide = ui.available_width() > 1000.0;
            let preview_w = if wide {
                (ui.available_width() - 380.0).min(820.0)
            } else {
                ui.available_width().min(720.0)
            };
            let layout = if wide {
                Layout::left_to_right(Align::Min)
            } else {
                Layout::top_down(Align::Min)
            };
            ui.with_layout(layout, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(preview_w);
                    preview(app, ui, preview_w);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| toggles(app, ui));
                });
                ui.add_space(16.0);
                // Its own column: inside the row, a card would lay its contents out sideways.
                ui.vertical(|ui| details(app, ui, host));
            });
        });
}

fn preview(app: &VeilApp, ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
    let tile = Tile {
        name: app.name.trim(),
        video: app.preview_video(),
        muted: !app.media.mic_on(),
        speaking: app.media.mic_on() && app.level > 0.45,
        hand_raised: false,
        is_you: true,
    };
    widgets::tile(ui, rect, &tile);
    let note = if !app.want_camera {
        Some("Your camera is off")
    } else if app.media.camera_error().is_some() {
        Some("Camera unavailable")
    } else if app.preview.is_none() {
        Some("Starting camera…")
    } else {
        None
    };
    if let Some(note) = note {
        ui.painter().text(
            rect.center() + vec2(0.0, rect.height() * 0.3),
            egui::Align2::CENTER_CENTER,
            note,
            egui::FontId::proportional(14.0),
            MUTED,
        );
    }
}

fn toggles(app: &mut VeilApp, ui: &mut egui::Ui) {
    let mic = ControlState {
        off: !app.want_mic,
        ..Default::default()
    };
    let label = if app.want_mic { "Mute" } else { "Unmute" };
    if control(ui, Icon::Mic, label, "Microphone on or off", mic).clicked() {
        app.want_mic = !app.want_mic;
        app.media.set_mic(app.want_mic);
    }
    let cam = ControlState {
        off: !app.want_camera,
        ..Default::default()
    };
    let label = if app.want_camera {
        "Stop video"
    } else {
        "Start video"
    };
    if control(ui, Icon::Video, label, "Camera on or off", cam).clicked() {
        app.want_camera = !app.want_camera;
        app.media.set_camera(app.want_camera);
    }
}

fn details(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    card().show(ui, |ui| {
        ui.set_width(320.0);
        ui.label(RichText::new(app.name.trim()).font(theme::bold(18.0)));
        ui.add_space(4.0);
        device(ui, Icon::Video, "Camera", &camera_status(app));
        device(ui, Icon::Mic, "Microphone", &mic_status(app));
        if app.media.mic_on() {
            level_meter(ui, app.level, 280.0);
        }
        device(ui, Icon::Volume, "Speaker", "Default communications device");
        ui.add_space(12.0);
        let action = if host { "Start call" } else { "Join now" };
        if pill(ui, Some(Icon::Video), action, Tone::Primary, 280.0).clicked() {
            app.go_call(host);
        }
        if pill(ui, None, "Back", Tone::Quiet, 280.0).clicked() {
            app.go_home();
        }
        ui.add_space(6.0);
        let note = if host {
            "You let people in from the lobby."
        } else {
            "The host lets you in."
        };
        ui.label(RichText::new(note).small().color(MUTED));
    });
}

fn device(ui: &mut egui::Ui, icon: Icon, kind: &str, status: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        icon.paint(ui.painter(), rect.center(), 15.0, MUTED);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(kind).small().color(MUTED));
            ui.label(status);
        });
    });
}

fn camera_status(app: &VeilApp) -> String {
    if let Some(error) = app.media.camera_error() {
        return error;
    }
    if !app.want_camera {
        return String::from("Off");
    }
    app.media
        .camera_name()
        .unwrap_or_else(|| String::from("Starting…"))
}

fn mic_status(app: &VeilApp) -> String {
    if let Some(error) = app.media.mic_error() {
        return error;
    }
    if app.want_mic {
        String::from("Default communications microphone")
    } else {
        String::from("Muted")
    }
}
