//! Before joining: see yourself, check the microphone, choose whether to come in muted or
//! with the camera off. One column: the preview, then a dock under it with the call bar's own
//! Mute, Video and Effects buttons and their menus (`controls::media`), then Start and Back.
//! A host whose firewall would block guests gets an Allow button (`crate::firewall`), and
//! before their first hosted call is asked once whether Veil may open a port on the router
//! (`engine/router.rs`).
//!
//! Invariants:
//! - The dock is the call bar's controls, so a setting chosen here looks and works the same
//!   once the call starts. Nothing here opens a pop-up from inside another pop-up.
//! - The preview shrinks to keep the dock and Start in the window; below the smallest preview
//!   the page scrolls.

use eframe::egui::{self, Align, CornerRadius, Layout, RichText, Sense, Vec2, vec2};

use crate::app::VeilApp;
use crate::firewall::{Profile, State};
use crate::icons::Icon;
use crate::screens::controls;
use crate::theme::{self, MUTED};
use crate::widgets::{self, Tile, Tone, pill};

/// The title and the space under it.
const HEADER_H: f32 = 46.0;
/// The space between the preview and the dock.
const GAP: f32 = 12.0;
/// The dock: one row of call bar buttons and its margins.
const DOCK_H: f32 = 78.0;
const DOCK_MARGIN_X: f32 = 12.0;
/// The note under the dock and the space around it.
const BELOW_H: f32 = 40.0;
const MIN_PREVIEW_W: f32 = 320.0;
const MAX_PREVIEW_W: f32 = 1200.0;
const START_W: f32 = 140.0;
const BACK_W: f32 = 96.0;
/// Space between buttons in the dock.
const DOCK_SPACING: f32 = 6.0;
/// The space either side of the line between the media buttons and the actions.
const DIVIDER_PAD: f32 = 10.0;
/// The actions: Back, then Start.
const ACTIONS_W: f32 = BACK_W + DOCK_SPACING + START_W;
/// One row inside the dock: media buttons, the line with its spacing, then the actions.
const ONE_ROW_W: f32 = controls::MEDIA_W + 2.0 * (DOCK_SPACING + DIVIDER_PAD) + 1.0 + ACTIONS_W;

/// The preview's width: as large as the window allows with the title, dock and note still
/// in view, kept 16:9. Never wider than the window.
fn preview_width(space: Vec2) -> f32 {
    let by_height = (space.y - HEADER_H - GAP - DOCK_H - BELOW_H) * 16.0 / 9.0;
    space
        .x
        .min(by_height)
        .clamp(MIN_PREVIEW_W, MAX_PREVIEW_W)
        .min(space.x)
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
            let width = preview_width(space);
            // The column's height depends on the notes under the dock, so it is measured and
            // used to centre it next frame.
            let height_id = ui.make_persistent_id("prejoin-height");
            let last: f32 = ui.ctx().data(|d| d.get_temp(height_id)).unwrap_or(0.0);
            let top = ((space.y - last) / 2.0).max(0.0);
            egui::ScrollArea::vertical()
                .id_salt("prejoin-page")
                .show(ui, |ui| {
                    ui.add_space(top);
                    let column = ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ui.allocate_ui_with_layout(
                            vec2(width, 0.0),
                            // One centre line for the title, preview, dock and notes.
                            Layout::top_down(Align::Center),
                            |ui| {
                                ui.set_width(width);
                                ui.label(RichText::new(title).font(theme::bold(26.0)));
                                ui.add_space(12.0);
                                preview(app, ui, width);
                                ui.add_space(GAP);
                                dock(app, ui, host);
                                ui.add_space(10.0);
                                below(app, ui, host);
                            },
                        )
                    });
                    let height = column.response.rect.height();
                    if (height - last).abs() > 0.5 {
                        ui.ctx().data_mut(|d| d.insert_temp(height_id, height));
                        ui.ctx().request_repaint();
                    }
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

/// The dock under the preview, as wide as its contents and centred: the call bar's media
/// buttons, a thin line, then Back and Start. When the window is too narrow for one row, the
/// actions go on a second row, each row centred.
fn dock(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    let one_row = ui.available_width() >= ONE_ROW_W + 2.0 * DOCK_MARGIN_X;
    let inner_w = if one_row {
        ONE_ROW_W
    } else {
        controls::MEDIA_W.max(ACTIONS_W)
    };
    // A box of the dock's exact width, so the centred column can centre it.
    ui.allocate_ui_with_layout(
        vec2(inner_w + 2.0 * DOCK_MARGIN_X, 0.0),
        Layout::top_down(Align::Center),
        |ui| {
            egui::Frame::new()
                .fill(theme::SURFACE)
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin::symmetric(DOCK_MARGIN_X as i8, 10))
                .show(ui, |ui| {
                    ui.set_width(inner_w);
                    if one_row {
                        row(ui, ONE_ROW_W, |ui| {
                            controls::media(app, ui);
                            divider(ui);
                            actions(app, ui, host);
                        });
                    } else {
                        row(ui, controls::MEDIA_W, |ui| controls::media(app, ui));
                        ui.add_space(8.0);
                        row(ui, ACTIONS_W, |ui| actions(app, ui, host));
                    }
                });
        },
    );
}

/// A row `width` wide, centred in the dock.
fn row(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        vec2(width, 0.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = DOCK_SPACING;
            add(ui);
        },
    );
}

/// The thin upright line between the media buttons and the actions.
fn divider(ui: &mut egui::Ui) {
    ui.add_space(DIVIDER_PAD);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 36.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::LINE);
    ui.add_space(DIVIDER_PAD);
}

/// Back, then Start: the main action last, as on a Mac.
fn actions(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    if pill(ui, None, "Back", Tone::Quiet, BACK_W).clicked() {
        app.go_home();
    }
    let start = if host { "Start call" } else { "Join now" };
    if pill(ui, Some(Icon::Video), start, Tone::Primary, START_W).clicked() {
        app.go_call(host);
    }
}

/// Who lets you in, then a host's firewall and router questions when they apply.
fn below(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    let note = match (app.demo, host) {
        (true, _) => "Other people are simulated.",
        (false, true) => "You let people in from the lobby.",
        (false, false) => "The host lets you in.",
    };
    ui.label(RichText::new(note).small().color(MUTED));
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

    fn fits(space: Vec2, width: f32) -> bool {
        HEADER_H + width * 9.0 / 16.0 + GAP + DOCK_H + BELOW_H <= space.y + 0.5
    }

    #[test]
    fn a_large_window_gets_the_largest_preview() {
        assert_eq!(preview_width(vec2(2400.0, 1400.0)), MAX_PREVIEW_W);
    }

    #[test]
    fn a_short_window_shrinks_the_preview_to_keep_the_dock_in_view() {
        let space = vec2(1800.0, 700.0);
        let width = preview_width(space);
        assert!(width < MAX_PREVIEW_W);
        assert!(fits(space, width));
    }

    #[test]
    fn a_narrow_window_never_gets_a_wider_preview() {
        assert_eq!(preview_width(vec2(300.0, 900.0)), 300.0);
        assert_eq!(preview_width(vec2(500.0, 900.0)), 500.0);
    }

    #[test]
    fn a_tiny_window_keeps_the_smallest_preview_and_scrolls() {
        assert_eq!(preview_width(vec2(1000.0, 300.0)), MIN_PREVIEW_W);
    }
}
