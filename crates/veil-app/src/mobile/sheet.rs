//! The phone's bottom sheet, in the calm Apple style: a dimmed page, a rounded panel rising
//! from the bottom, a grab handle, a centred title and a Done link. Replaces desktop windows
//! on the phone. Tapping the dimmed page or Done closes it.
//!
//! Invariant: the sheet draws above everything else and swallows taps behind it, so a tap
//! meant for the sheet never reaches the call underneath.
use super::widgets as w;
use crate::theme;
use eframe::egui::{self, Align, Align2, Color32, CornerRadius, Layout, Rect, Sense, vec2};

/// Shows a sheet titled `title`. Returns false once the person closes it.
pub fn show(ctx: &egui::Context, id: &str, title: &str, draw: impl FnOnce(&mut egui::Ui)) -> bool {
    let screen = ctx.content_rect();
    let mut open = true;
    egui::Area::new(egui::Id::unique(id).with("scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let scrim = ui.allocate_rect(screen, Sense::click());
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
            if scrim.clicked() {
                open = false;
            }
        });
    egui::Area::new(egui::Id::unique(id).with("sheet"))
        .order(egui::Order::Tooltip)
        .anchor(Align2::CENTER_BOTTOM, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let width = screen.width();
            ui.set_width(width);
            egui::Frame::new()
                .fill(theme::SURFACE)
                .corner_radius(CornerRadius { nw: 24, ne: 24, sw: 0, se: 0 })
                .inner_margin(egui::Margin { left: 24, right: 24, top: 8, bottom: 32 })
                .show(ui, |ui| {
                    ui.set_width(width - 48.0);
                    let (handle, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
                    ui.painter().rect_filled(
                        Rect::from_center_size(handle.center(), vec2(36.0, 5.0)),
                        3.0,
                        theme::MUTED,
                    );
                    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), w::TARGET), Sense::hover());
                    ui.painter().text(
                        bar.center(),
                        Align2::CENTER_CENTER,
                        title,
                        w::font(ui, 17.0, true),
                        theme::TEXT,
                    );
                    let mut done = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(bar)
                            .layout(Layout::right_to_left(Align::Center)),
                    );
                    if w::link(&mut done, None, "Done").clicked() {
                        open = false;
                    }
                    ui.add_space(8.0);
                    ui.with_layout(Layout::top_down(Align::Center), draw);
                });
        });
    open
}
