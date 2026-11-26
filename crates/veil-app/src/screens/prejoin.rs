//! Before joining: see yourself, check the microphone moves the meter, choose whether to come
//! in muted or with the camera off. A host whose firewall would block guests gets an Allow
//! button (`crate::firewall`), and before their first hosted call is asked once whether Veil
//! may open a port on the router (`engine/router.rs`).

use eframe::egui::{self, Align, Layout, Rect, RichText, Sense, UiBuilder, Vec2, vec2};

use crate::app::VeilApp;
use crate::firewall::{Profile, State};
use crate::icons::Icon;
use crate::screens::controls::MIRROR_HINT;
use crate::theme::{self, MUTED};
use crate::widgets::{
    self, ControlState, Tile, Tone, card, control, device_error as status, device_picker as picker,
    level_meter, pill,
};

/// Above this width the preview and the card sit side by side.
const WIDE_FROM: f32 = 1000.0;
const GAP: f32 = 16.0;
/// The title and the space under it.
const HEADER_H: f32 = 46.0;
/// The space under the preview and the row of buttons.
const CONTROLS_H: f32 = 66.0;
const MIN_PREVIEW_W: f32 = 320.0;
const MAX_PREVIEW_W: f32 = 1200.0;
/// The card's size until it has been measured, on the first frame.
const CARD_GUESS: Vec2 = vec2(322.0, 730.0);
/// The width inside the card. Every picker, slider, line and button in it is this wide, so
/// they share one right edge (the shared pickers are 280 wide).
const CARD_W: f32 = 280.0;
const PICKER_W: f32 = CARD_W;
/// The card's margins and border, top and bottom together (`widgets::card`).
const CARD_FRAME_H: f32 = 42.0;
/// The card's parts that never scroll: its margins, your name, Start, Back and the note.
const FIXED_H: f32 = 200.0;
/// The settings stay at least this tall, however short the window; the page scrolls then.
const MIN_SCROLL_H: f32 = 160.0;

/// Where the side-by-side layout goes in the space available.
#[derive(Debug, PartialEq)]
struct WidePlan {
    preview_w: f32,
    offset: Vec2,
}

/// Makes the preview as large as the window allows, then centres the title, preview and card
/// together. A block too large for the window starts at its top left corner.
fn wide_plan(space: Vec2, card: Vec2, gap: f32) -> WidePlan {
    let tallest = (space.y - HEADER_H - CONTROLS_H) * 16.0 / 9.0;
    let preview_w = (space.x - card.x - gap)
        .min(tallest)
        .clamp(MIN_PREVIEW_W, MAX_PREVIEW_W);
    let column_h = preview_w * 9.0 / 16.0 + CONTROLS_H;
    let block = vec2(preview_w + gap + card.x, HEADER_H + column_h.max(card.y));
    WidePlan {
        preview_w,
        offset: ((space - block) / 2.0).max(Vec2::ZERO),
    }
}

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
            let space = ui.available_size();
            if space.x > WIDE_FROM {
                wide(app, ui, host, title, space);
            } else {
                // Stacked, the page itself scrolls, so the card never needs to.
                egui::ScrollArea::vertical()
                    .id_salt("prejoin-page")
                    .show(ui, |ui| {
                        heading(ui, title);
                        let preview_w = space.x.min(720.0);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            preview_column(app, ui, preview_w);
                            ui.add_space(GAP);
                            ui.vertical(|ui| details(app, ui, host, None));
                        });
                    });
            }
        });
}

fn heading(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).font(theme::bold(26.0)));
    ui.add_space(12.0);
}

fn wide(app: &mut VeilApp, ui: &mut egui::Ui, host: bool, title: &str, space: Vec2) {
    // The card's height depends on what it shows, so it is measured and used next frame.
    let id = ui.scope_id().with("prejoin-card-size");
    let card = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(CARD_GUESS);
    let gap = GAP + ui.spacing().item_spacing.x;
    let plan = wide_plan(space, card, gap);
    let area = Rect::from_min_size(
        ui.available_rect_before_wrap().min + plan.offset,
        space - plan.offset,
    );
    ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
        heading(ui, title);
        let measured = ui
            .with_layout(Layout::left_to_right(Align::Min), |ui| {
                let column_h = preview_column(app, ui, plan.preview_w);
                ui.add_space(GAP);
                // Its own column: inside the row, a card would lay its contents out sideways.
                // As tall as the preview and its buttons, so both columns share a top and a
                // bottom edge; the settings scroll inside it instead.
                let outer_h = column_h.max(FIXED_H + MIN_SCROLL_H);
                ui.vertical(|ui| details(app, ui, host, Some(outer_h - CARD_FRAME_H)))
                    .response
                    .rect
                    .size()
            })
            .inner;
        if (measured - card).length() > 0.5 {
            ui.ctx().data_mut(|d| d.insert_temp(id, measured));
            ui.ctx().request_repaint();
        }
    });
}

/// The preview and its buttons. Returns the column's height.
fn preview_column(app: &mut VeilApp, ui: &mut egui::Ui, width: f32) -> f32 {
    ui.vertical(|ui| {
        ui.set_width(width);
        preview(app, ui, width);
        ui.add_space(8.0);
        ui.horizontal(|ui| toggles(app, ui));
    })
    .response
    .rect
    .height()
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
    let mirrored = app.media.mirror();
    let mirror = ControlState {
        selected: mirrored,
        ..Default::default()
    };
    if control(ui, Icon::Mirror, "Mirror", MIRROR_HINT, mirror).clicked() {
        app.media.set_mirror(!mirrored);
    }
}

/// The card: your name, the settings in sections, then Start and Back. Given an inner
/// `height` (side by side), the card is exactly that tall: Start and Back sit on its bottom
/// edge and the settings scroll in the space between. Without one (stacked), it is as tall as
/// its contents and the page scrolls.
fn details(app: &mut VeilApp, ui: &mut egui::Ui, host: bool, height: Option<f32>) {
    card().show(ui, |ui| {
        ui.set_width(CARD_W);
        ui.label(RichText::new(app.name.trim()).font(theme::bold(18.0)));
        ui.add_space(4.0);
        let Some(height) = height else {
            settings(app, ui, host);
            ui.add_space(12.0);
            actions(app, ui, host);
            return;
        };
        ui.set_height(height);
        // Bottom up: the actions claim the bottom edge first, the settings fill what is left.
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(RichText::new(lobby_note(app, host)).small().color(MUTED));
            ui.add_space(6.0);
            if pill(ui, None, "Back", Tone::Quiet, CARD_W).clicked() {
                app.go_home();
            }
            if pill(
                ui,
                Some(Icon::Video),
                start_label(host),
                Tone::Primary,
                CARD_W,
            )
            .clicked()
            {
                app.go_call(host);
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("prejoin-settings")
                    .auto_shrink([false, false])
                    .show(ui, |ui| settings(app, ui, host));
            });
        });
    });
}

fn settings(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    video_section(app, ui);
    audio_section(app, ui);
    connection_section(app, ui, host);
}

fn actions(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    if pill(
        ui,
        Some(Icon::Video),
        start_label(host),
        Tone::Primary,
        CARD_W,
    )
    .clicked()
    {
        app.go_call(host);
    }
    if pill(ui, None, "Back", Tone::Quiet, CARD_W).clicked() {
        app.go_home();
    }
    ui.add_space(6.0);
    ui.label(RichText::new(lobby_note(app, host)).small().color(MUTED));
}

fn start_label(host: bool) -> &'static str {
    if host { "Start call" } else { "Join now" }
}

fn lobby_note(app: &VeilApp, host: bool) -> &'static str {
    match (app.demo, host) {
        (true, _) => "Other people are simulated.",
        (false, true) => "You let people in from the lobby.",
        (false, false) => "The host lets you in.",
    }
}

/// A section title, with a thin line above every section but the first.
fn section(ui: &mut egui::Ui, title: &str, first: bool) {
    if !first {
        ui.add_space(10.0);
        let (rect, _) = ui.allocate_exact_size(vec2(PICKER_W, 1.0), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme::LINE);
        ui.add_space(6.0);
    }
    ui.label(RichText::new(title).font(theme::bold(14.0)));
    ui.add_space(2.0);
}

fn video_section(app: &mut VeilApp, ui: &mut egui::Ui) {
    section(ui, "Video", true);
    let chosen = app.media.choice().camera.clone();
    if let Some(id) = picker(
        ui,
        Icon::Video,
        "Camera",
        &app.devices.cameras,
        chosen.as_deref(),
        "Default camera",
    ) {
        app.media.choose_camera(id);
    }
    status(ui, app.media.camera_error());
    crate::screens::effects::look_picker(app, ui);
    crate::screens::effects::blur_picker(app, ui);
}

fn audio_section(app: &mut VeilApp, ui: &mut egui::Ui) {
    section(ui, "Audio", false);
    let choice = app.media.choice().clone();
    if let Some(id) = picker(
        ui,
        Icon::Mic,
        "Microphone",
        &app.devices.mics,
        choice.mic.as_deref(),
        "System default",
    ) {
        app.media.choose_mic(id);
    }
    status(ui, app.media.mic_error());
    if app.media.mic_on() {
        level_meter(ui, app.level, PICKER_W);
    }
    if let Some(id) = picker(
        ui,
        Icon::Volume,
        "Speaker",
        &app.devices.speakers,
        choice.speaker.as_deref(),
        "System default",
    ) {
        app.media.choose_speaker(id);
    }
}

fn connection_section(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    section(ui, "Connection", false);
    if let Some(choice) = widgets::data_use_picker(ui, app.data_use.get()) {
        app.set_data_use(choice);
    }
    if host && !app.demo {
        firewall(app, ui);
        router_question(app, ui);
    }
}

/// For a host: whether guests can reach you, and the button that lets them.
fn firewall(app: &VeilApp, ui: &mut egui::Ui) {
    let state = app.firewall.state();
    // The button needs the kind of network a rule is for. A failed check never guesses it: a
    // Private rule clicked on a Public network would change nothing and keep failing.
    let (note, profile) = match &state {
        State::Unknown | State::Allowed => return,
        State::Checking => ("Checking your firewall…", None),
        State::Asking => ("Confirm in the Windows prompt", None),
        State::Blocked(Profile::Public) => ("This network is set to Public", Some(Profile::Public)),
        State::Blocked(profile) => ("Your firewall may block guests", Some(*profile)),
        State::Failed(_) => ("Could not allow Veil", app.firewall.profile()),
    };
    let warn = matches!(state, State::Blocked(_) | State::Failed(_));
    ui.add_space(8.0);
    let colour = if warn { theme::WARN } else { MUTED };
    let label = ui.label(RichText::new(note).small().color(colour));
    if let State::Failed(why) = &state {
        let help = if profile.is_some() {
            why.clone()
        } else {
            format!(
                "{why}. Veil could not tell which kind of network this is. In Windows \
                 Defender Firewall, allow inbound UDP for Veil on the network type shown in \
                 Windows Settings > Network."
            )
        };
        label.on_hover_text(help);
    }
    if let Some(profile) = profile {
        let (text, tip) = if profile == Profile::Public {
            (
                "Allow on Public networks",
                "Windows treats this network as Public, like a café or hotel. If it is your \
                 home or office, set it to Private in Windows Settings > Network instead. \
                 Allowing Public lets anyone on any Public network try to reach Veil while \
                 you host. Windows asks you to confirm.",
            )
        } else {
            (
                "Allow guests",
                "Adds one Windows firewall rule so guests can reach this call: Veil only, \
                 network calls only, this kind of network only. Windows asks you to confirm.",
            )
        };
        if pill(ui, Some(Icon::Shield), text, Tone::Quiet, 280.0)
            .on_hover_text(tip)
            .clicked()
        {
            app.firewall.allow(profile, ui.ctx());
        }
    }
}

/// Asked once, before the first call you host: may Veil open a port on your router? The
/// answer is saved either way and can be changed in Settings > Connection.
fn router_question(app: &mut VeilApp, ui: &mut egui::Ui) {
    if app.connection.saved.asked_open_port || app.connection.saved.local_only {
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

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: Vec2 = vec2(360.0, 700.0);

    #[test]
    fn a_large_window_centres_a_large_preview() {
        let plan = wide_plan(vec2(2400.0, 1300.0), CARD, 24.0);
        assert_eq!(plan.preview_w, MAX_PREVIEW_W);
        assert_eq!(plan.offset.x, (2400.0 - (1200.0 + 24.0 + 360.0)) / 2.0);
        assert!(plan.offset.y > 0.0);
    }

    #[test]
    fn a_short_window_limits_the_preview_by_height() {
        let plan = wide_plan(vec2(1800.0, 700.0), CARD, 24.0);
        assert!(plan.preview_w * 9.0 / 16.0 + HEADER_H + CONTROLS_H <= 700.5);
        assert!(
            plan.offset.x > 0.0,
            "the spare width is shared on both sides"
        );
    }

    #[test]
    fn a_block_too_large_for_the_window_starts_at_the_corner() {
        let plan = wide_plan(vec2(1001.0, 500.0), vec2(360.0, 900.0), 24.0);
        assert_eq!(plan.offset.y, 0.0);
        assert!(plan.preview_w >= MIN_PREVIEW_W);
    }
}
