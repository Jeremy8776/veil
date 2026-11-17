//! The call bar's own controls: microphone and camera (each with a device menu), Effects,
//! Share and Raise. The panel buttons and Leave stay in `call.rs`.
//!
//! Purpose: during a call you can pick another microphone, speaker or camera and set the
//! microphone and speaker volume without leaving, from the small arrow beside Mute and beside
//! Stop video, as in Zoom or Teams. The menus reuse the pre-join pickers
//! (`widgets::device_picker`), and the choice goes through `LocalMedia`, so the call follows:
//! a new microphone or camera reopens its capture thread (the engine's taps stay), and a new
//! speaker or speaker volume reaches the playback thread through `volume::Playback`.
//!
//! Invariants:
//! - Muting closes the microphone and stopping video closes the camera (`media.rs`); picking
//!   a device while it is off only remembers it, nothing opens.
//! - The device lists are read again each time a menu button is clicked, so a headset
//!   plugged in during the call shows up.

use eframe::egui::{self, CornerRadius, Sense, Stroke, pos2, vec2};

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::screens::call::CallView;
use crate::theme::{self, MUTED, RAISED, RAISED_HOVER, TEXT};
use crate::volume::{MIC_MAX, SPEAKER_MAX};
use crate::widgets::{self, ControlState, control, device_error, device_picker, level_meter};

/// Width of the arrow beside Mute and Stop video.
pub const MENU_W: f32 = 18.0;
/// The bar's buttons drawn here: Mute, Stop video, Effects, Share, Raise.
pub const CONTROLS: usize = 5;
/// The arrows drawn here.
pub const MENUS: usize = 2;
const MENU_INNER: f32 = 280.0;

pub fn show(view: &mut CallView, app: &mut VeilApp, ui: &mut egui::Ui) {
    let mic_on = app.media.mic_on();
    let state = ControlState {
        off: !mic_on,
        ..Default::default()
    };
    let label = if mic_on { "Mute" } else { "Unmute" };
    if control(ui, Icon::Mic, label, "Microphone (Ctrl+Shift+M)", state).clicked() {
        toggle_mic(app);
    }
    let audio = menu_button(ui, "Audio settings", "Microphone, speaker and volume");
    reload_on_open(app, &audio);
    menu(&audio, |ui| audio_menu(app, ui));

    let cam_on = app.media.camera_on();
    let state = ControlState {
        off: !cam_on,
        ..Default::default()
    };
    let label = if cam_on { "Stop video" } else { "Start video" };
    if control(ui, Icon::Video, label, "Camera (Ctrl+Shift+O)", state).clicked() {
        toggle_camera(app);
    }
    let video = menu_button(ui, "Video settings", "Choose a camera");
    reload_on_open(app, &video);
    menu(&video, |ui| video_menu(app, ui));

    let effects = control(
        ui,
        Icon::Effects,
        "Effects",
        "Filters and background blur",
        ControlState::default(),
    );
    menu(&effects, |ui| {
        widgets::filter_picker(ui, &app.media);
        widgets::blur_picker(ui, &app.media);
    });
    let share = control(
        ui,
        Icon::Share,
        "Share",
        "Share your screen",
        ControlState::default(),
    );
    if share.clicked() {
        view.notify("Screen sharing is not built yet.");
    }
    let raised = view.call.you().is_some_and(|p| p.hand_raised);
    let state = ControlState {
        selected: raised,
        ..Default::default()
    };
    let label = if raised { "Lower" } else { "Raise" };
    if control(ui, Icon::Hand, label, "Raise or lower your hand", state).clicked()
        && let Some(you) = view.call.you_mut()
    {
        you.hand_raised = !raised;
    }
}

/// A popup above the bar, opened and closed by `button`.
fn menu(button: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::from_toggle_button_response(button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP)
        .show(|ui| {
            ui.set_width(MENU_INNER);
            add(ui);
        });
}

/// Reads the device lists again when a menu button is clicked.
/// shortcut: listing runs on the interface thread (as the pre-join screen does when it
/// opens) and can take a moment; move it to a worker if a slow driver makes clicks stutter.
fn reload_on_open(app: &mut VeilApp, button: &egui::Response) {
    if button.clicked() {
        app.devices = crate::devices::DeviceLists::load();
    }
}

fn audio_menu(app: &mut VeilApp, ui: &mut egui::Ui) {
    let choice = app.media.choice().clone();
    if let Some(id) = device_picker(
        ui,
        Icon::Mic,
        "Microphone",
        &app.devices.mics,
        choice.mic.as_deref(),
        "System default",
    ) {
        app.media.choose_mic(id);
    }
    device_error(ui, app.media.mic_error());
    let mut mic = app.media.mic_volume();
    if volume(
        ui,
        "Mic volume",
        &mut mic,
        MIC_MAX,
        "Above 100% boosts a quiet microphone",
    ) {
        app.media.set_mic_volume(mic);
    }
    if app.media.mic_on() {
        level_meter(ui, app.level, MENU_INNER);
    }
    ui.add_space(6.0);
    if let Some(id) = device_picker(
        ui,
        Icon::Volume,
        "Speaker",
        &app.devices.speakers,
        choice.speaker.as_deref(),
        "System default",
    ) {
        app.media.choose_speaker(id);
    }
    let mut speaker = app.media.speaker_volume();
    let hint = "How loud the call plays. Your system volume still applies.";
    if volume(ui, "Speaker volume", &mut speaker, SPEAKER_MAX, hint) {
        app.media.set_speaker_volume(speaker);
    }
}

fn video_menu(app: &mut VeilApp, ui: &mut egui::Ui) {
    let chosen = app.media.choice().camera.clone();
    if let Some(id) = device_picker(
        ui,
        Icon::Video,
        "Camera",
        &app.devices.cameras,
        chosen.as_deref(),
        "Default camera",
    ) {
        app.media.choose_camera(id);
    }
    device_error(ui, app.media.camera_error());
}

/// A volume slider in percent. Returns true when it moved.
fn volume(ui: &mut egui::Ui, label: &str, gain: &mut f32, max: f32, hint: &str) -> bool {
    ui.label(egui::RichText::new(label).small().color(MUTED));
    let mut shown = percent(*gain);
    let changed = ui
        .add(egui::Slider::new(&mut shown, 0..=percent(max)).suffix("%"))
        .on_hover_text(hint)
        .changed();
    if changed {
        *gain = f32::from(shown) / 100.0;
    }
    changed
}

/// A gain as a whole percentage, for the sliders.
pub fn percent(gain: f32) -> u16 {
    (gain.clamp(0.0, 6.0) * 100.0).round() as u16
}

/// The small arrow beside a control that opens its device menu.
fn menu_button(ui: &mut egui::Ui, label: &str, hint: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(MENU_W, 58.0), Sense::click());
    let response = response.on_hover_text(hint);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if response.hovered() {
            RAISED_HOVER
        } else {
            RAISED
        };
        painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
        if response.has_focus() {
            let stroke = Stroke::new(2.0, theme::ACCENT);
            painter.rect_stroke(rect, theme::RADIUS, stroke, egui::StrokeKind::Inside);
        }
        // An upward chevron: the menu opens above the bar.
        let c = rect.center();
        let stroke = Stroke::new(1.6, TEXT);
        painter.line_segment([pos2(c.x - 4.0, c.y + 2.0), pos2(c.x, c.y - 2.0)], stroke);
        painter.line_segment([pos2(c.x, c.y - 2.0), pos2(c.x + 4.0, c.y + 2.0)], stroke);
    }
    response
}

pub fn toggle_mic(app: &mut VeilApp) {
    let on = !app.media.mic_on();
    app.want_mic = on;
    app.media.set_mic(on);
}

pub fn toggle_camera(app: &mut VeilApp) {
    let on = !app.media.camera_on();
    app.want_camera = on;
    app.media.set_camera(on);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_show_as_whole_percentages() {
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(MIC_MAX), 200);
        assert_eq!(percent(0.333), 33);
        assert_eq!(percent(-1.0), 0);
    }
}
