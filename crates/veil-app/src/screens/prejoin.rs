//! Before joining: see yourself, check the microphone moves the meter, choose whether to come
//! in muted or with the camera off. A host whose firewall would block guests gets an Allow
//! button (`crate::firewall`), and before their first hosted call is asked once whether Veil
//! may open a port on the router (`engine/router.rs`).

use eframe::egui::{self, Align, Layout, RichText, Sense, vec2};

use crate::app::VeilApp;
use crate::firewall::State;
use crate::icons::Icon;
use crate::screens::controls::MIRROR_HINT;
use crate::theme::{self, MUTED};
use crate::widgets::{
    self, ControlState, Tile, Tone, card, control, device_error as status, device_picker as picker,
    level_meter, pill,
};

pub fn show(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::PAGE)
                .inner_margin(egui::Margin::same(32)),
        )
        .show(ui, |ui| {
            let title = match (app.demo, host) {
                (true, _) => "Try a demo call",
                (false, true) => "Start a call",
                (false, false) => "Join a call",
            };
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
    let mirror = ControlState {
        selected: app.mirror_self,
        ..Default::default()
    };
    if control(ui, Icon::Video, "Mirror", MIRROR_HINT, mirror).clicked() {
        app.mirror_self = !app.mirror_self;
    }
}

fn details(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    card().show(ui, |ui| {
        ui.set_width(320.0);
        ui.label(RichText::new(app.name.trim()).font(theme::bold(18.0)));
        ui.add_space(4.0);
        let choice = app.media.choice().clone();
        let lists = &app.devices;
        if let Some(id) = picker(
            ui,
            Icon::Video,
            "Camera",
            &lists.cameras,
            choice.camera.as_deref(),
            "Default camera",
        ) {
            app.media.choose_camera(id);
        }
        status(ui, app.media.camera_error());
        widgets::filter_picker(ui, &app.media);
        widgets::blur_picker(ui, &app.media);
        if let Some(id) = picker(
            ui,
            Icon::Mic,
            "Microphone",
            &lists.mics,
            choice.mic.as_deref(),
            "System default",
        ) {
            app.media.choose_mic(id);
        }
        status(ui, app.media.mic_error());
        if app.media.mic_on() {
            level_meter(ui, app.level, 280.0);
        }
        if let Some(id) = picker(
            ui,
            Icon::Volume,
            "Speaker",
            &lists.speakers,
            choice.speaker.as_deref(),
            "System default",
        ) {
            app.media.choose_speaker(id);
        }
        if host && !app.demo {
            firewall(app, ui);
            router_question(app, ui);
        }
        ui.add_space(12.0);
        let action = if host { "Start call" } else { "Join now" };
        if pill(ui, Some(Icon::Video), action, Tone::Primary, 280.0).clicked() {
            app.go_call(host);
        }
        if pill(ui, None, "Back", Tone::Quiet, 280.0).clicked() {
            app.go_home();
        }
        ui.add_space(6.0);
        let note = match (app.demo, host) {
            (true, _) => "Other people are simulated.",
            (false, true) => "You let people in from the lobby.",
            (false, false) => "The host lets you in.",
        };
        ui.label(RichText::new(note).small().color(MUTED));
    });
}

/// For a host: whether guests can reach you, and the button that lets them.
fn firewall(app: &VeilApp, ui: &mut egui::Ui) {
    let (note, button) = match app.firewall.state() {
        State::Unknown | State::Allowed => return,
        State::Checking => ("Checking your firewall…", false),
        State::Asking => ("Confirm in the Windows prompt", false),
        State::Blocked => ("Your firewall may block guests", true),
        State::Failed(_) => ("Could not allow Veil", true),
    };
    ui.add_space(8.0);
    let colour = if button { theme::WARN } else { MUTED };
    let label = ui.label(RichText::new(note).small().color(colour));
    if let State::Failed(why) = app.firewall.state() {
        label.on_hover_text(why);
    }
    if button {
        let tip = "Adds one Windows firewall rule so guests can reach this call: Veil only, \
                   network calls only. Windows asks you to confirm.";
        if pill(ui, Some(Icon::Shield), "Allow guests", Tone::Quiet, 280.0)
            .on_hover_text(tip)
            .clicked()
        {
            app.firewall.allow(ui.ctx());
        }
    }
}

/// Asked once, before the first call you host: may Veil open a port on your router? The
/// answer is saved either way and can be changed in Settings > Connection.
fn router_question(app: &mut VeilApp, ui: &mut egui::Ui) {
    if app.connection.saved.asked_open_port {
        return;
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new("Open a port on your router?")
            .small()
            .color(MUTED),
    )
    .on_hover_text(super::connection::ROUTER_HELP);
    let mut answer = None;
    ui.horizontal(|ui| {
        let half = (280.0 - ui.spacing().item_spacing.x) / 2.0;
        if pill(ui, None, "Allow", Tone::Quiet, half)
            .on_hover_text(super::connection::ROUTER_HELP)
            .clicked()
        {
            answer = Some(true);
        }
        if pill(ui, None, "Not now", Tone::Quiet, half)
            .on_hover_text("Change it any time in Settings > Connection.")
            .clicked()
        {
            answer = Some(false);
        }
    });
    if let Some(yes) = answer {
        let mut settings = app.connection.saved.clone();
        settings.open_port = yes;
        settings.asked_open_port = true;
        if let Err(problems) = app.connection.save(settings.clone()) {
            // A hand-edited file with a bad field: keep the answer for this run anyway.
            crate::note!("router choice not saved: {problems:?}");
            app.connection.saved = settings;
        }
    }
}
