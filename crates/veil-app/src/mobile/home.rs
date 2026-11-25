//! Phone Home in two parts: the logo and title centred in the upper space, and every control
//! together at the bottom, in thumb reach. Start a call is the one filled button; Join is an
//! inline arrow and nearby search a text link. Connection settings and the connection report
//! sit behind an icon in the top-right corner, so there is no top bar and no footer.
use super::{EDGE, action, page, widgets as w};
use crate::app::{Screen, VeilApp};
use crate::icons::Icon;
use crate::theme;
use crate::widgets::Tone;
use eframe::egui::{self, Align2, Margin, Rect, UiBuilder, vec2};

/// The logo block's height: 112 logo, 24 gap, the wordmark.
/// shortcut: a fixed estimate used to centre it; measure the block if its content changes.
const HERO: f32 = 160.0;

pub fn home(app: &mut VeilApp, ui: &mut egui::Ui) {
    controls(app, ui);
    page(ui, "mobile-home", |ui| {
        let corner = Rect::from_min_size(
            ui.max_rect().right_top() + vec2(-w::TARGET, 0.0),
            vec2(w::TARGET, w::TARGET),
        );
        ui.add_space(((ui.available_height() - HERO) * 0.5).max(w::TARGET));
        ui.vertical_centered(|ui| {
            crate::logo::show(ui, 112.0, theme::PAGE);
            ui.add_space(24.0);
            wordmark(ui);
        });
        connection_menu(app, ui, corner);
    });
}

/// "VEIL" in light capitals with wide letter spacing, so it reads as a mark, not a heading.
fn wordmark(ui: &mut egui::Ui) {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "VEIL",
        0.0,
        egui::TextFormat {
            font_id: w::font(ui, 20.0, false),
            color: theme::TEXT,
            extra_letter_spacing: 10.0,
            ..Default::default()
        },
    );
    ui.label(job);
}

/// Every control in one group at the bottom. Spacing stays on an 8 point grid.
fn controls(app: &mut VeilApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("mobile-home-controls")
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(Margin {
            left: EDGE,
            right: EDGE,
            top: 8,
            bottom: 16,
        }))
        .show_separator_line(false)
        .show(ui, |ui| {
            field(ui, &mut app.name, "Your name");
            if action(ui, Some(Icon::Video), "Start a call", Tone::Primary) {
                crate::screens::home::start(app, true);
            }
            ui.add_space(16.0);
            if invite_row(app, ui) {
                join(app);
            }
            if let Some(error) = &app.home_error {
                w::label(ui, error, 13.0, false, theme::WARN);
            }
            if let Some(error) = crate::media::lock(&app.keys).problem.clone() {
                w::label(ui, error, 13.0, false, theme::WARN);
            }
            let keys = app.keys.clone();
            if let Some(invite) = app
                .nearby
                .ui(ui, |device| crate::media::lock(&keys).is_verified(device))
            {
                app.joining = Some(invite);
                crate::screens::home::start(app, false);
            }
        });
}

/// The connection icon sits in the page's top-right corner, over empty space beside the logo,
/// so it needs no row of its own. Its tooltip names the current route.
fn connection_menu(app: &mut VeilApp, ui: &mut egui::Ui, corner: Rect) {
    let mut ui = ui.new_child(UiBuilder::new().id_salt("mobile-home-connection").max_rect(corner));
    let route = if app.connection.saved.local_only {
        "Connection: local network only"
    } else {
        "Connection: internet and local calls"
    };
    let menu = w::icon(&mut ui, Icon::Signal, route, false);
    egui::Popup::menu(&menu).show(|ui| {
        ui.set_min_width(240.0);
        if action(ui, Some(Icon::Signal), "Connection settings", Tone::Quiet) {
            app.screen = Screen::Connection(Box::new(crate::screens::connection::Form::new(
                &app.connection,
            )));
            ui.close();
        }
        if action(ui, Some(Icon::Info), "Connection report", Tone::Quiet) {
            app.diagnostics.open();
            ui.close();
        }
    });
}

/// A filled pill field with no outline. Its corners match the pill buttons, so the
/// controls share one shape.
fn boxed<R>(ui: &mut egui::Ui, right: i8, draw: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .corner_radius(24)
        .inner_margin(Margin {
            left: 20,
            right,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| ui.horizontal(draw).inner)
        .inner
}

fn text<'a>(value: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .frame(egui::Frame::NONE)
        .align(Align2::LEFT_CENTER)
        .hint_text(hint)
        .char_limit(veil_security::invite::MAX_LINK_LEN)
}

fn field(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    boxed(ui, 20, |ui| {
        ui.add_sized([ui.available_width(), w::TARGET], text(value, hint));
    });
}

/// The invite box with Paste and an inline Join arrow. True when the person asks to join.
fn invite_row(app: &mut VeilApp, ui: &mut egui::Ui) -> bool {
    let ready = !app.invite.trim().is_empty();
    let (pasted, joined) = boxed(ui, 0, |ui| {
        let gap = ui.spacing().item_spacing.x;
        let width = (ui.available_width() - 2.0 * (w::TARGET + gap)).max(1.0);
        let edit = ui.add_sized(
            [width, w::TARGET],
            text(&mut app.invite, "Paste an invite link"),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let pasted = w::icon(ui, Icon::Copy, "Paste invite", false).clicked();
        let go = ui.add_enabled_ui(ready, w::go).inner.clicked();
        (pasted, ready && (go || enter))
    });
    #[cfg(target_os = "android")]
    if pasted {
        match veil_media::ffi::android::paste_text() {
            Ok(value) => app.invite = value,
            Err(error) => app.home_error = Some(error.to_string()),
        }
    }
    let _ = pasted;
    joined
}

fn join(app: &mut VeilApp) {
    match veil_security::short_invite::InviteLink::parse(
        app.invite.trim(),
        crate::screens::invite::now_unix(),
    ) {
        Ok(invite) => {
            app.joining = Some(invite);
            crate::screens::home::start(app, false);
        }
        Err(error) => app.home_error = Some(format!("Check the invitation: {error}")),
    }
}
