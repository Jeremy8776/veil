//! The home screen's tools under the main card: connection, logs, device key, demo call and
//! the update check, as a grid of framed buttons. The update switch also shows the version.
//!
//! Invariants:
//! - "New device key" asks once before it replaces the key: the first click arms it (shown in
//!   the warning colour), the second replaces it.
//! - Every button gives egui its role and label, so screen readers read it as a button or a
//!   switch, not as painted shapes.

use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Role, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use super::home::{HomeHelp, HomeHelpState, track_help};
use crate::app::VeilApp;
use crate::icons::Icon;
use crate::theme::{ACCENT, LINE, MUTED, RADIUS, RAISED, SURFACE, TEXT, WARN};

const HEIGHT: f32 = 40.0;
const GAP: f32 = 8.0;

/// The tools grid, `width` wide.
pub(super) fn show(app: &mut VeilApp, ui: &mut Ui, width: f32, help: &mut HomeHelpState) {
    let wide = width >= 700.0;
    let columns = if wide { 3.0 } else { 2.0 };
    let cell = (width - GAP * (columns - 1.0)) / columns;
    let mut problem = None;
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        ui.horizontal(|ui| {
            connection(app, ui, cell, help);
            logs(app, ui, cell, help);
            if wide {
                problem = copy_device_key(app, ui, cell, help);
            }
        });
        ui.horizontal(|ui| {
            if wide {
                new_device_key(app, ui, cell, help);
                demo(app, ui, cell, help);
                updates(app, ui, cell, help);
            } else {
                problem = copy_device_key(app, ui, cell, help);
                new_device_key(app, ui, cell, help);
            }
        });
        if !wide {
            ui.horizontal(|ui| {
                demo(app, ui, cell, help);
                updates(app, ui, cell, help);
            });
        }
    });
    if let Some(problem) = problem {
        ui.label(RichText::new(problem).small().color(WARN));
    }
    if ui
        .link("Connection report")
        .on_hover_text("Review local evidence before copying a redacted report (Ctrl+Shift+E).")
        .clicked()
    {
        app.diagnostics.open();
    }
}

fn demo(app: &mut VeilApp, ui: &mut Ui, width: f32, help: &mut HomeHelpState) {
    let response = button(ui, Icon::Play, "Demo call", false, width)
        .on_hover_text("Try the interface with simulated people. Nothing is sent.");
    track_help(&response, HomeHelp::Demo, help);
    if response.clicked() {
        app.demo = true;
        super::home::start(app, true);
    }
}

fn connection(app: &mut VeilApp, ui: &mut Ui, width: f32, help: &mut HomeHelpState) {
    let response = button(ui, Icon::Signal, "Connection", false, width)
        .on_hover_text("Local networks, nearby discovery, your own servers and a fixed port.");
    track_help(&response, HomeHelp::Connection, help);
    if response.clicked() {
        app.nearby.stop();
        let form = super::connection::Form::new(&app.connection);
        app.screen = crate::app::Screen::Connection(Box::new(form));
    }
}

fn logs(app: &mut VeilApp, ui: &mut Ui, width: f32, help: &mut HomeHelpState) {
    let response = button(ui, Icon::Folder, "Logs", false, width).on_hover_text(
        "Opens the folder with Veil's log files. If something goes wrong, send veil.log, \
         or veil.previous.log if Veil froze or closed. They hold states, timings and \
         errors: no names, chat or invite links.",
    );
    track_help(&response, HomeHelp::Logs, help);
    if response.clicked() {
        app.home_error = app.log_opener.start(ui.ctx()).err();
    }
}

fn updates(app: &mut VeilApp, ui: &mut Ui, width: f32, help: &mut HomeHelpState) {
    let on = app.updates.enabled();
    let mut detail = format!("v{}", crate::update::VERSION);
    if app.updates.status() == crate::update::Status::UpToDate {
        detail.push_str(" · up to date");
    }
    let local_only = app.connection.saved.local_only;
    let response = ui
        .add_enabled_ui(!local_only, |ui| {
            switch(
                ui,
                Icon::Refresh,
                "Check for updates",
                &detail,
                on && !local_only,
                width,
            )
        })
        .inner
        .on_hover_text(
            "Asks GitHub once per start whether a newer Veil is out. GitHub sees your IP \
         address. Nothing is downloaded unless you choose Install.",
        )
        .on_disabled_hover_text("Off while Local only is on.");
    track_help(&response, HomeHelp::Updates, help);
    if response.clicked() && !local_only {
        app.updates.set_enabled(!on, ui.ctx());
    }
}

/// Copy the public device key. Returns any trouble saving the device key.
fn copy_device_key(
    app: &mut VeilApp,
    ui: &mut Ui,
    width: f32,
    help: &mut HomeHelpState,
) -> Option<String> {
    let (full, problem) = {
        let keys = crate::media::lock(&app.keys);
        let public = keys.public();
        (veil_net::node_addr::key_text(&public), keys.problem.clone())
    };
    let copy = button(ui, Icon::Copy, "Copy device key", false, width).on_hover_text(
        "Puts this device's key on the clipboard. Send it to someone who runs a veil-node, \
         so they can let you host calls through it. It is public: it identifies this device, \
         and cannot be used to sign in as you.",
    );
    track_help(&copy, HomeHelp::DeviceKey, help);
    if copy.clicked() {
        ui.ctx().copy_text(full);
    }
    problem
}

/// Replace the device key only after the existing two-click confirmation.
fn new_device_key(app: &mut VeilApp, ui: &mut Ui, width: f32, help: &mut HomeHelpState) {
    let short = crate::engine::short(&crate::media::lock(&app.keys).public());
    let armed = app.confirm_new_key;
    let label = if armed {
        "Confirm new key"
    } else {
        "New device key"
    };
    let renew = button(ui, Icon::Key, label, armed, width).on_hover_text(format!(
        "This device is {short}. A new key means people who verified you see a new \
         device. Use it if you think your key was copied."
    ));
    track_help(&renew, HomeHelp::NewKey, help);
    if renew.clicked() {
        if armed {
            crate::media::lock(&app.keys).renew();
        }
        app.confirm_new_key = !armed;
    }
}

/// A framed button with an icon and a label. `warn` draws it in the warning colour.
fn button(ui: &mut Ui, icon: Icon, label: &str, warn: bool, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if ui.is_rect_visible(rect) {
        let (fg, icon_colour) = frame(ui, rect, &response, warn);
        label_with_icon(ui, rect, icon, label, fg, icon_colour);
    }
    response
}

/// A framed button with an icon, a label and an on/off switch at the right. The caller flips
/// the setting on click.
fn switch(ui: &mut Ui, icon: Icon, label: &str, detail: &str, on: bool, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::click());
    response
        .widget_info(|| WidgetInfo::selected(Role::Switch, true, on, format!("{label}, {detail}")));
    if ui.is_rect_visible(rect) {
        let (fg, icon_colour) = frame(ui, rect, &response, false);
        label_with_icon(ui, rect, icon, detail, fg, icon_colour);
        let track =
            Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), vec2(28.0, 16.0));
        let (fill, knob_x) = if on {
            (ACCENT, track.right() - 8.0)
        } else {
            (LINE, track.left() + 8.0)
        };
        let painter = ui.painter();
        painter.rect_filled(track, CornerRadius::same(8), fill);
        painter.circle_filled(pos2(knob_x, track.center().y), 5.5, Color32::WHITE);
    }
    response
}

/// Paints the button's background, border and focus ring. Returns the text and icon colours.
fn frame(ui: &Ui, rect: Rect, response: &Response, warn: bool) -> (Color32, Color32) {
    let hovered = response.hovered();
    let fill = if hovered { RAISED } else { SURFACE };
    let border = if warn { WARN } else { LINE };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(RADIUS),
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            RADIUS + 2,
            Stroke::new(2.0, TEXT),
            StrokeKind::Outside,
        );
    }
    match (warn, hovered) {
        (true, _) => (WARN, WARN),
        (false, true) => (TEXT, TEXT),
        (false, false) => (TEXT, MUTED),
    }
}

fn label_with_icon(
    ui: &Ui,
    rect: Rect,
    icon: Icon,
    label: &str,
    fg: Color32,
    icon_colour: Color32,
) {
    let painter = ui.painter();
    let font = FontId::proportional(13.5);
    let text_width = painter
        .layout_no_wrap(label.to_owned(), font.clone(), fg)
        .size()
        .x;
    let left = rect.center().x - (24.0 + text_width) / 2.0;
    icon.paint(
        painter,
        pos2(left + 7.5, rect.center().y),
        15.0,
        icon_colour,
    );
    painter.text(
        pos2(left + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
}
