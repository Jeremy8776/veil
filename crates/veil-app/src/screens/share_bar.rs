//! The floating "you are sharing" bar, shown over other apps until hidden or stopped.
//!
//! Purpose: lets you move the bar by its handle, change what you share, hide the bar or stop,
//! wherever you are working. While the share includes computer sound, the bar also holds a
//! Sound switch (mute for everyone), a volume slider and the level going out.
//!
//! Layout: one centred row of fixed-width groups split by thin dividers, as on pre-join:
//! handle and status, then sound, then Change, Hide and Stop last. Every button is
//! [`BUTTON_W`] by [`BUTTON_H`]. The window is sized to the row ([`window_size`]).
//!
//! Invariants:
//! - On Windows the bar is kept out of screen captures, so people watching a share of the whole
//!   screen do not see it. You still do.
//! - Change only opens the picker. What you share stays the same until you choose something
//!   else, and Cancel leaves it alone. Those watching never see the picker (`share.rs`).
//!
//! shortcut: the bar assumes the screen holding Veil starts at the desktop's origin, so on some
//! multi-screen layouts it opens on another screen. It can be dragged.

use std::time::Duration;

use eframe::egui::{
    self, Align, CursorIcon, Layout, Response, Sense, Stroke, Ui, ViewportBuilder, ViewportCommand,
    ViewportId, pos2, vec2,
};

use crate::app::VeilApp;
use crate::screens::call_menus;
use crate::screens::share::dock::sound_switch;
use crate::share::ShareBar;
use crate::theme::{self, MUTED, TEXT};
use crate::volume::SPEAKER_MAX;
use crate::widgets::{Tone, named_level_meter, pill_sized};

/// Also how the window is found to keep it out of captures.
const TITLE: &str = "Veil is sharing your screen";
const HEIGHT: f32 = 56.0;
/// Space between the row and the window's left and right edges.
const EDGE: f32 = 16.0;
const BUTTON_W: f32 = 100.0;
const BUTTON_H: f32 = 36.0;
const GAP: f32 = theme::GAP;
const HANDLE_W: f32 = 18.0;
/// The red dot and "Sharing".
const STATUS_W: f32 = 84.0;
const SLIDER_W: f32 = 96.0;
const METER_W: f32 = 40.0;
/// Extra space either side of a divider, on top of the usual gap.
const DIVIDER_PAD: f32 = 4.0;
const DIVIDER_W: f32 = 1.0 + 2.0 * DIVIDER_PAD + GAP;
/// How far above the bottom of the screen the bar opens.
const MARGIN: f32 = 96.0;
const RETRY: Duration = Duration::from_millis(100);

/// The row's width: handle and status, the sound group when `sound`, then three buttons.
fn row_width(sound: bool) -> f32 {
    let status = HANDLE_W + GAP + STATUS_W;
    let sound = if sound {
        DIVIDER_W + GAP + BUTTON_W + GAP + SLIDER_W + GAP + METER_W
    } else {
        0.0
    };
    let buttons = DIVIDER_W + GAP + 3.0 * BUTTON_W + 2.0 * GAP;
    status + sound + buttons
}

/// The bar window's size: the row and its edges.
fn window_size(sound: bool) -> egui::Vec2 {
    vec2(row_width(sound) + 2.0 * EDGE, HEIGHT)
}

pub fn show(app: &VeilApp, ctx: &egui::Context) {
    if !app.share.is_sharing() || app.share.bar_hidden() || ctx.embed_viewports() {
        return;
    }
    let size = window_size(app.share.sound.state().is_on());
    let monitor = ctx
        .input(|i| i.viewport().monitor_size)
        .unwrap_or(vec2(1_920.0, 1_080.0));
    let builder = ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size(size)
        .with_position(pos2(
            (monitor.x - size.x) / 2.0,
            monitor.y - size.y - MARGIN,
        ))
        .with_decorations(false)
        .with_resizable(false)
        .with_taskbar(false)
        .with_always_on_top();
    let control = app.share.bar();
    ctx.show_viewport_deferred(
        ViewportId::from_hash_of("veil-share-bar"),
        builder,
        move |ui, _class| contents(ui, &control),
    );
}

/// Six dots that say the bar can be dragged from here.
fn handle(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(HANDLE_W, 32.0), Sense::drag());
    let colour = if response.hovered() || response.dragged() {
        TEXT
    } else {
        MUTED
    };
    for column in [-3.0, 3.0] {
        for row in [-6.0, 0.0, 6.0] {
            ui.painter()
                .circle_filled(rect.center() + vec2(column, row), 1.5, colour);
        }
    }
    response
        .on_hover_cursor(CursorIcon::Grab)
        .on_hover_text("Move")
}

/// A red dot and "Sharing", draggable like the handle.
fn status(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(STATUS_W, BUTTON_H), Sense::drag());
    let painter = ui.painter();
    let dot = pos2(rect.left() + 5.0, rect.center().y);
    painter.circle_filled(dot, 5.0, theme::DANGER);
    painter.text(
        pos2(rect.left() + 18.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Sharing",
        theme::bold(14.0),
        TEXT,
    );
    response.on_hover_cursor(CursorIcon::Grab).on_hover_text(
        "Only you see this bar. It is kept out of what you share, and hiding it does not \
         stop the share.",
    )
}

/// The thin upright line between groups.
fn divider(ui: &mut Ui) {
    ui.add_space(DIVIDER_PAD);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::LINE);
    ui.add_space(DIVIDER_PAD);
}

fn button(ui: &mut Ui, label: &str, tone: Tone, hint: &str) -> Response {
    pill_sized(ui, None, label, tone, BUTTON_W, BUTTON_H).on_hover_text(hint)
}

fn contents(ui: &mut Ui, control: &ShareBar) {
    let ctx = ui.ctx().clone();
    if control.keep_out_of_capture(TITLE) {
        ctx.request_repaint_after(RETRY);
    }
    let frame = egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::LINE));
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        let sound = control.sound();
        let width = row_width(sound.is_on());
        let row = vec2(ui.available_width(), ui.available_height());
        ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            let grip = handle(ui);
            let label = status(ui);
            if grip.drag_started() || label.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if sound.is_on() {
                divider(ui);
                let muted = sound.muted();
                let hint = if muted {
                    "Send your computer's sound again"
                } else {
                    "Your computer's sound goes out with your share. Click to mute it for \
                     everyone; your screen stays shared."
                };
                let switch = sound_switch(ui, !muted, vec2(BUTTON_W, BUTTON_H));
                if switch.on_hover_text(hint).clicked() {
                    sound.set_muted(!muted);
                }
                let mut level = sound.level();
                let slider =
                    call_menus::slider(ui, &mut level, SPEAKER_MAX, SLIDER_W, "Share sound volume")
                        .on_hover_text(format!(
                            "How loud your computer's sound is for everyone: {}%",
                            call_menus::percent(level)
                        ));
                if slider.changed() {
                    sound.set_level(level);
                }
                let meter = if muted { 0.0 } else { sound.meter() };
                named_level_meter(ui, meter, METER_W, "Shared sound level")
                    .on_hover_text("Sound going out");
                // The meter moves only while it is drawn again.
                ctx.request_repaint_after(Duration::from_millis(200));
            }
            divider(ui);
            let mut close = ui.input(|i| i.viewport().close_requested());
            if button(ui, "Change", Tone::Quiet, "Share something else").clicked() {
                control.change();
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
                ctx.request_repaint_of(ViewportId::ROOT);
            }
            let hide = "Hide this bar. You keep sharing; change or stop it from the call window.";
            if button(ui, "Hide", Tone::Quiet, hide).clicked() {
                close = true;
            }
            if button(ui, "Stop", Tone::Danger, "Stop sharing").clicked() {
                control.stop();
                close = true;
            }
            if close {
                control.hide();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_row_and_its_edges() {
        for sound in [false, true] {
            assert_eq!(window_size(sound).x, row_width(sound) + 2.0 * EDGE);
            assert_eq!(window_size(sound).y, HEIGHT);
        }
    }

    #[test]
    fn sound_adds_its_group_and_one_divider() {
        let group = DIVIDER_W + GAP + BUTTON_W + GAP + SLIDER_W + GAP + METER_W;
        assert!((row_width(true) - row_width(false) - group).abs() < 0.01);
    }

    #[test]
    fn the_bar_fits_a_small_laptop_screen() {
        assert!(window_size(true).x <= 1_024.0);
    }
}
