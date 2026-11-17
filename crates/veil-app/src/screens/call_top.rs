//! The bar across the top of a call: name, time and the encryption badge on the left;
//! people, Invite, stats and the view switch on the right.
//!
//! Purpose: a calm header as in Teams. Short labels, the detail in tooltips.
//!
//! Invariants:
//! - Icons are painted with `Icon::paint`, never as text: the icon font is not in the text
//!   fonts, so a glyph in a label shows as an empty box.
//! - Every clickable piece reports a label to screen readers.

use std::time::Instant;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, Role, Sense,
    Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use crate::icons::Icon;
use crate::model::clock;
use crate::screens::call::{CallView, View, toggle};
use crate::screens::invite;
use crate::screens::panels::Panel;
use crate::theme::{self, LINE, MUTED, RAISED, RAISED_HOVER, SPEAKING, SURFACE, TEXT};
use crate::widgets;

const CHIP_H: f32 = 30.0;

pub fn show(view: &mut CallView, ui: &mut Ui, now: Instant) {
    egui::Panel::top("call_top")
        .exact_size(52.0)
        .frame(
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, LINE))
                .inner_margin(egui::Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(egui::RichText::new(&view.call.title).font(theme::bold(15.0)));
                divider(ui);
                time(ui, &clock(view.call.elapsed(now)));
                let open = view.panel == Some(Panel::Security);
                let green = Look {
                    fg: SPEAKING,
                    fill: SPEAKING.gamma_multiply(0.14),
                    hover: SPEAKING.gamma_multiply(0.24),
                    ring: open,
                };
                let hint =
                    "End-to-end encrypted. Show safety numbers and how each person is connected.";
                if chip(ui, Some(Icon::Lock), "Encrypted", green)
                    .on_hover_text(hint)
                    .clicked()
                {
                    view.panel = toggle(view.panel, Panel::Security);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    right_side(view, ui);
                });
            });
        });
}

fn right_side(view: &mut CallView, ui: &mut Ui) {
    let (icon, label, next) = match view.view {
        View::Gallery => (Icon::Speaker, "Speaker view", View::Speaker),
        View::Speaker => (Icon::Grid, "Gallery view", View::Gallery),
    };
    if widgets::icon_button(ui, icon, label).clicked() {
        view.view = next;
    }
    let tip = "Call stats: delay, loss, bit rate (Ctrl+Shift+D)";
    if widgets::icon_button(ui, Icon::Signal, tip).clicked() {
        view.stats = !view.stats;
    }
    divider(ui);
    if view.call.is_host && !view.call.demo {
        let lifetime = invite::lifetime_text(&view.call, invite::now_unix());
        let invite = chip(ui, Some(Icon::Link), "Invite", Look::quiet(false))
            .on_hover_text(format!("Copy the invite link. {lifetime}"));
        if invite.clicked() {
            view.copy_invite(ui.ctx());
        }
    }
    let count = view.call.participants.len();
    let open = view.panel == Some(Panel::People);
    if chip(
        ui,
        Some(Icon::People),
        &count.to_string(),
        Look::quiet(open),
    )
    .on_hover_text(people_text(count))
    .clicked()
    {
        view.panel = toggle(view.panel, Panel::People);
    }
}

/// "1 person" or "3 people", for the People chip's tooltip.
fn people_text(count: usize) -> String {
    if count == 1 {
        String::from("1 person in the call")
    } else {
        format!("{count} people in the call")
    }
}

/// The call time in a fixed-width box, so the badge beside it does not move as digits change.
fn time(ui: &mut Ui, text: &str) {
    let font = FontId::proportional(13.5);
    let widest: String = text
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    let w = ui
        .painter()
        .layout_no_wrap(widest, font.clone(), MUTED)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(w + 2.0, CHIP_H), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("Call time {text}")));
    ui.painter()
        .text(rect.left_center(), Align2::LEFT_CENTER, text, font, MUTED);
}

/// A short upright line between groups.
fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, LINE);
}

/// How a chip is coloured.
#[derive(Clone, Copy)]
struct Look {
    fg: Color32,
    fill: Color32,
    hover: Color32,
    /// Its panel is open: an outline in the text colour.
    ring: bool,
}

impl Look {
    fn quiet(open: bool) -> Self {
        Self {
            fg: TEXT,
            fill: if open { RAISED_HOVER } else { RAISED },
            hover: RAISED_HOVER,
            ring: false,
        }
    }
}

/// A rounded button with an optional icon and a short label.
fn chip(ui: &mut Ui, icon: Option<Icon>, label: &str, look: Look) -> Response {
    let font = FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, look.fg);
    let icon_w = if icon.is_some() { 20.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 24.0, CHIP_H);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let radius = CornerRadius::same((CHIP_H / 2.0) as u8);
    let fill = if response.hovered() {
        look.hover
    } else {
        look.fill
    };
    painter.rect_filled(rect, radius, fill);
    if look.ring {
        painter.rect_stroke(rect, radius, Stroke::new(1.0, look.fg), StrokeKind::Inside);
    }
    if response.has_focus() {
        let ring = Rect::from_center_size(rect.center(), rect.size() + vec2(4.0, 4.0));
        painter.rect_stroke(
            ring,
            radius,
            Stroke::new(2.0, theme::ACCENT),
            StrokeKind::Outside,
        );
    }
    let mut x = rect.left() + 12.0;
    if let Some(icon) = icon {
        icon.paint(painter, pos2(x + 7.0, rect.center().y), 14.0, look.fg);
        x += icon_w;
    }
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        look.fg,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_count_reads_naturally() {
        assert_eq!(people_text(1), "1 person in the call");
        assert_eq!(people_text(3), "3 people in the call");
    }
}
