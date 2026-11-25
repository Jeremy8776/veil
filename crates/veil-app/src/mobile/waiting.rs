//! The phone's call stage while nobody else has joined, in the calm Apple style: your own
//! picture fills the space, with a short title and round invite controls underneath.
//! No card and no explanatory text; detail lives in tooltips.
use super::widgets as w;
use crate::app::VeilApp;
use crate::icons::Icon;
use crate::model::CallStatus;
use crate::screens::call::CallView;
use crate::theme;
use eframe::egui::{self, Align, Layout, Rect, Sense, UiBuilder, vec2};

/// Width of one round control with its label, as in the call bar.
const CONTROL: f32 = 88.0;

pub(super) fn show(view: &mut CallView, app: &VeilApp, ui: &mut egui::Ui, area: Rect) {
    let mut picture = area;
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::bottom_up(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(8.0);
            if view.call.is_host {
                if !view.mobile_invite {
                    if view.call.invite_full.is_empty() {
                        w::label(ui, "Preparing invite…", 15.0, false, theme::MUTED);
                    } else {
                        controls(view, ui);
                    }
                    ui.add_space(16.0);
                    code(view, ui);
                }
                w::label(ui, "Your call is ready", 20.0, true, theme::TEXT);
            } else {
                let title = if view.call.status == CallStatus::Lobby {
                    "Waiting for the host"
                } else {
                    "Reaching the host"
                };
                ui.add(egui::Spinner::new().size(20.0).color(theme::MUTED));
                ui.add_space(8.0);
                w::label(ui, title, 20.0, true, theme::TEXT).on_hover_text(
                    if view.call.status == CallStatus::Lobby {
                        "The host will decide when to let you in."
                    } else {
                        "Finding a connection to your call."
                    },
                );
            }
            ui.add_space(24.0);
            picture = Rect::from_min_max(area.min, egui::pos2(area.right(), ui.cursor().bottom()));
        },
    );
    if picture.height() < 48.0 {
        return;
    }
    let Some(you) = view.call.participants.iter().find(|person| person.is_you) else {
        return;
    };
    crate::widgets::tile(
        ui,
        picture,
        &crate::widgets::Tile {
            name: &you.name,
            video: crate::screens::bento::video(app, you),
            muted: you.muted,
            speaking: you.speaking,
            hand_raised: you.hand_raised,
            is_you: true,
        },
    );
}

/// The nearby code on one centred line, when nearby discovery is on.
pub(super) fn code(view: &CallView, ui: &mut egui::Ui) {
    if view.call.nearby_code.is_empty() {
        return;
    }
    w::label(
        ui,
        format!("Nearby code {}", view.call.nearby_code),
        15.0,
        true,
        theme::TEXT,
    )
    .on_hover_text("Nearby devices type this code to request the invite.");
    ui.add_space(8.0);
}

/// Copy and QR as round controls with short labels, centred like the call bar.
pub(super) fn controls(view: &mut CallView, ui: &mut egui::Ui) {
    let size = vec2(CONTROL * 2.0, 72.0);
    let (row, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(Align::Min)),
    );
    row.spacing_mut().item_spacing.x = 0.0;
    let copy = row
        .allocate_ui_with_layout(vec2(CONTROL, 72.0), Layout::top_down(Align::Center), |ui| {
            w::call_control(ui, Icon::Copy, "Copy invite", false, false)
                .on_hover_text("Anyone with the invite can ask to join. You still decide who gets in.")
        })
        .inner;
    if copy.clicked() {
        super::copy(row.ctx(), &view.call.invite_full);
        view.notify("Invite copied");
    }
    let qr = row
        .allocate_ui_with_layout(vec2(CONTROL, 72.0), Layout::top_down(Align::Center), |ui| {
            w::call_control(ui, Icon::Grid, "QR code", false, false)
        })
        .inner;
    if qr.clicked() {
        crate::invite_qr::open(row.ctx());
    }
}
