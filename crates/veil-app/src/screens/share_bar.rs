//! The floating "you are sharing" bar, shown over other apps until hidden or stopped.
//!
//! Purpose: lets you move the bar by its handle, change what you share, hide the bar or stop,
//! wherever you are working.
//!
//! Invariants:
//! - On Windows the bar is kept out of screen captures, so people watching a share of the whole
//!   screen do not see it. You still do.
//! - Change only opens the picker. What you share stays the same until you choose something
//!   else, and Cancel leaves it alone. Those watching see a still picture while it is open.
//!
//! shortcut: the bar assumes the screen holding Veil starts at the desktop's origin, so on some
//! multi-screen layouts it opens on another screen. It can be dragged.

use std::time::Duration;

use eframe::egui::{
    self, Align, CursorIcon, Layout, Response, RichText, Sense, Ui, ViewportBuilder,
    ViewportCommand, ViewportId, pos2, vec2,
};

use crate::app::VeilApp;
use crate::share::ShareBar;
use crate::theme::{self, MUTED, TEXT};
use crate::widgets::{Tone, pill};

/// Also how the window is found to keep it out of captures.
const TITLE: &str = "Veil is sharing your screen";
const SIZE: egui::Vec2 = vec2(540.0, 56.0);
/// How far above the bottom of the screen the bar opens.
const MARGIN: f32 = 96.0;
const RETRY: Duration = Duration::from_millis(100);

pub fn show(app: &VeilApp, ctx: &egui::Context) {
    if !app.share.is_sharing() || app.share.bar_hidden() || ctx.embed_viewports() {
        return;
    }
    let monitor = ctx
        .input(|i| i.viewport().monitor_size)
        .unwrap_or(vec2(1_920.0, 1_080.0));
    let builder = ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size(SIZE)
        .with_position(pos2(
            (monitor.x - SIZE.x) / 2.0,
            monitor.y - SIZE.y - MARGIN,
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
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 32.0), Sense::drag());
    let colour = if response.hovered() || response.dragged() { TEXT } else { MUTED };
    for column in [-3.0, 3.0] {
        for row in [-6.0, 0.0, 6.0] {
            ui.painter().circle_filled(rect.center() + vec2(column, row), 1.5, colour);
        }
    }
    response.on_hover_cursor(CursorIcon::Grab).on_hover_text("Move")
}

fn contents(ui: &mut Ui, control: &ShareBar) {
    let ctx = ui.ctx().clone();
    if control.keep_out_of_capture(TITLE) {
        ctx.request_repaint_after(RETRY);
    }
    let frame = egui::Frame::new()
        .fill(theme::SURFACE)
        .inner_margin(egui::Margin::symmetric(14, 8));
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        ui.horizontal_centered(|ui| {
            let grip = handle(ui);
            let text = RichText::new("Sharing your screen").color(TEXT);
            let label = ui.add(egui::Label::new(text).sense(Sense::drag()));
            if grip.drag_started() || label.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            let mut close = ui.input(|i| i.viewport().close_requested());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, None, "Hide", Tone::Quiet, 70.0).clicked() {
                    close = true;
                }
                if pill(ui, None, "Stop sharing", Tone::Danger, 130.0).clicked() {
                    control.stop();
                    close = true;
                }
                let change = pill(ui, None, "Change", Tone::Quiet, 90.0)
                    .on_hover_text("Share something else");
                if change.clicked() {
                    control.change();
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
                    ctx.request_repaint_of(ViewportId::ROOT);
                }
            });
            if close {
                control.hide();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        });
    });
}
