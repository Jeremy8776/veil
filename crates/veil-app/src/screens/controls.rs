//! The call bar's own controls: microphone and camera (each with a device menu), Effects,
//! Share (`share.rs`) and Raise. The panel buttons and Leave stay in `call.rs`.
//!
//! Purpose: during a call you can pick another microphone, speaker or camera and set the
//! microphone and speaker volume without leaving, from the small arrow beside Mute and beside
//! Stop video, as in Zoom or Teams. The menus are tick lists built from
//! `call_menus` (not the pre-join drop-downs, which cannot open inside a pop-up), and the choice goes through `LocalMedia`, so the call follows:
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
use crate::screens::call_menus;
use crate::screens::share::SharePicker;
use crate::theme::{self, RAISED, RAISED_HOVER};
use crate::volume::{MIC_MAX, SPEAKER_MAX};
use crate::widgets::{ControlState, chevron, control, device_error, level_meter};

/// Width of the arrow beside Mute and Stop video. It joins its button with no gap.
pub const MENU_W: f32 = 22.0;
/// The bar's buttons drawn here: Mute, Stop video, Effects, Share, Raise.
pub const CONTROLS: usize = 5;
/// The arrows drawn here.
pub const MENUS: usize = 2;
const MENU_INNER: f32 = 280.0;
/// Tooltips for the switches in the audio menu.
const NOISE_HINT: &str = "Removes fans, typing, traffic and other background sound from your \
     microphone. It runs on this computer, so your sound goes nowhere else.";
const AUTO_GAIN_HINT: &str = "Turns your voice up or down to a steady level, so a quiet \
     microphone is easy to hear. Learns only from speech.";
const CLEAN_OTHERS_HINT: &str = "Removes background sound from everyone else, on this computer. \
     Uses a little more processor for each person in the call.";
/// Shown wherever mirroring can be turned on or off.
pub const MIRROR_HINT: &str = "Flips your camera left to right, for you and everyone in the call. Your choice is remembered.";
/// The Auto row while Auto is saving data because the connection is slow.
const AUTO_SAVING: &str = "Auto: saving data";
const AUTO_SAVING_HINT: &str = "Your connection is slow, so Veil sends and asks for a smaller \
     picture, as Data saver does. It goes back to the best picture by itself when the \
     connection has been steady for a while.";

pub fn show(view: &mut CallView, app: &mut VeilApp, ui: &mut egui::Ui) {
    let mic_on = app.media.mic_on();
    let state = ControlState {
        off: !mic_on,
        split: true,
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
        split: true,
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
        "Filters, background blur and mirror",
        ControlState::default(),
    );
    menu(&effects, |ui| effects_menu(app, ui));
    let sharing = app.share.is_sharing();
    let state = ControlState {
        selected: sharing,
        ..Default::default()
    };
    let (label, hint) = if sharing {
        ("Stop share", "Stop sharing your screen")
    } else {
        ("Share", "Share a screen, a window or part of one")
    };
    if control(ui, Icon::Share, label, hint, state).clicked() {
        if sharing {
            app.share.stop();
        } else {
            view.picker = Some(SharePicker::open(app));
        }
    }
    let raised = view.call.you().is_some_and(|p| p.hand_raised);
    let state = ControlState {
        selected: raised,
        ..Default::default()
    };
    let label = if raised { "Lower" } else { "Raise" };
    if control(ui, Icon::Hand, label, "Raise or lower your hand", state).clicked() {
        view.call.raise_hand(!raised);
    }
}

/// A pop-up above the bar, opened and closed by `button`. Its contents must not open a
/// pop-up of their own (see `call_menus`).
fn menu(button: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::from_toggle_button_response(button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP)
        .gap(8.0)
        .frame(call_menus::frame())
        .show(|ui| {
            ui.set_width(MENU_INNER);
            ui.spacing_mut().item_spacing.y = 2.0;
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
    call_menus::section(ui, Icon::Mic, "Microphone");
    let mics = &app.devices.mics;
    if let Some(id) =
        call_menus::device_list(ui, "mics", mics, choice.mic.as_deref(), "System default")
    {
        app.media.choose_mic(id);
    }
    device_error(ui, app.media.mic_error());
    let mut mic = app.media.mic_volume();
    let hint = "Microphone volume, on top of Auto volume. Above 100% boosts a quiet microphone.";
    if call_menus::volume(ui, "Mic volume", &mut mic, MIC_MAX, hint) {
        app.media.set_mic_volume(mic);
    }
    if app.media.mic_on() {
        ui.add_space(2.0);
        level_meter(ui, app.level, ui.available_width());
    }
    ui.add_space(4.0);
    let auto = app.media.auto_gain();
    let button = call_menus::toggle_button(ui, Icon::Volume, "Auto volume", auto);
    if button.on_hover_text(AUTO_GAIN_HINT).clicked() {
        app.media.set_auto_gain(!auto);
    }
    ui.add_space(4.0);
    let suppress = app.media.noise_suppression();
    let button = call_menus::toggle_button(ui, Icon::Mic, "Noise suppression", suppress);
    if button.on_hover_text(NOISE_HINT).clicked() {
        app.media.set_noise_suppression(!suppress);
    }
    call_menus::divider(ui);
    call_menus::section(ui, Icon::Volume, "Speaker");
    let speakers = &app.devices.speakers;
    if let Some(id) = call_menus::device_list(
        ui,
        "speakers",
        speakers,
        choice.speaker.as_deref(),
        "System default",
    ) {
        app.media.choose_speaker(id);
    }
    let mut speaker = app.media.speaker_volume();
    let hint = "How loud the call plays. Your system volume still applies.";
    if call_menus::volume(ui, "Speaker volume", &mut speaker, SPEAKER_MAX, hint) {
        app.media.set_speaker_volume(speaker);
    }
    ui.add_space(4.0);
    let playback = app.media.playback();
    let clean = playback.clean_others.load(std::sync::atomic::Ordering::Relaxed);
    let button = call_menus::toggle_button(ui, Icon::Volume, "Remove noise from others", clean);
    if button.on_hover_text(CLEAN_OTHERS_HINT).clicked() {
        playback
            .clean_others
            .store(!clean, std::sync::atomic::Ordering::Relaxed);
    }
}

fn video_menu(app: &mut VeilApp, ui: &mut egui::Ui) {
    let chosen = app.media.choice().camera.clone();
    call_menus::section(ui, Icon::Video, "Camera");
    let cameras = &app.devices.cameras;
    if let Some(id) =
        call_menus::device_list(ui, "cameras", cameras, chosen.as_deref(), "Default camera")
    {
        app.media.choose_camera(id);
    }
    device_error(ui, app.media.camera_error());
    call_menus::divider(ui);
    call_menus::section(ui, Icon::Signal, "Data use");
    let current = app.data_use.get();
    let saving = app.data_use.auto_saving();
    for choice in crate::data_use::DataUse::ALL {
        let (label, help) = match choice {
            crate::data_use::DataUse::Auto if saving => (AUTO_SAVING, AUTO_SAVING_HINT),
            _ => (choice.label(), choice.help()),
        };
        let row = call_menus::choice(ui, label, choice == current);
        if row.on_hover_text(help).clicked() && choice != current {
            app.set_data_use(choice);
        }
    }
}

fn effects_menu(app: &VeilApp, ui: &mut egui::Ui) {
    call_menus::section(ui, Icon::Effects, "Background blur");
    crate::screens::effects::blur_menu(app, ui);
    call_menus::divider(ui);
    call_menus::section(ui, Icon::Effects, "Filter");
    crate::screens::effects::look_menu(app, ui);
    call_menus::divider(ui);
    let mirrored = app.media.mirror();
    let mirror = call_menus::toggle_button(ui, Icon::Mirror, "Mirror my camera", mirrored);
    if mirror.on_hover_text(MIRROR_HINT).clicked() {
        app.media.set_mirror(!mirrored);
    }
}

/// The arrow beside a control that opens its device menu: the right half of a split
/// button, joined to the control just drawn (`ControlState::split` leaves no gap after it).
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
        let corners = CornerRadius {
            nw: 0,
            sw: 0,
            ne: theme::RADIUS,
            se: theme::RADIUS,
        };
        painter.rect_filled(rect, corners, fill);
        painter.line_segment(
            [
                pos2(rect.left(), rect.top() + 14.0),
                pos2(rect.left(), rect.bottom() - 14.0),
            ],
            Stroke::new(1.0, theme::LINE),
        );
        if response.has_focus() {
            let stroke = Stroke::new(2.0, theme::ACCENT);
            painter.rect_stroke(rect, corners, stroke, egui::StrokeKind::Inside);
        }
        // An upward chevron, centred on the divider: the menu opens above the bar.
        chevron(painter, rect.center(), true, theme::MUTED);
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
