//! The share picker's dock: one centred row of equal buttons under the grid, as on pre-join.
//! Cancel and the step's own button first, then the computer sound switch, a thin divider and
//! Share (or Switch) last. A short hint for the area step sits centred above it.
//!
//! Invariants:
//! - Every button is [`BUTTON_W`] by [`BUTTON_H`], so the row reads as one group.
//! - The row is sized to its buttons and centred on the picker's centre line.
//! - Where this computer cannot capture its sound, the switch is greyed out, forced off, and
//!   its tooltip says why.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Response, RichText, Role, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};
use veil_media::screen;

use super::{Clicked, SharePicker, Step};
use crate::icons::Icon;
use crate::share::SoundSupport;
use crate::theme::{self, ACCENT, MUTED, RAISED, RAISED_HOVER, TEXT};
use crate::widgets::{Tone, pill_sized};

const BUTTON_W: f32 = 124.0;
const BUTTON_H: f32 = 40.0;
const GAP: f32 = theme::GAP;
/// Extra space either side of the divider before Share, on top of the usual gap.
const DIVIDER_PAD: f32 = 4.0;

/// The width of a dock of `buttons` buttons, the last set apart by the divider.
fn dock_width(buttons: usize) -> f32 {
    let buttons = buttons.max(1) as f32;
    // The divider replaces one gap and adds its own on the far side.
    buttons * BUTTON_W + (buttons - 2.0) * GAP + 2.0 * (GAP + DIVIDER_PAD) + 1.0
}

impl SharePicker {
    /// The hint for the area step, then the dock.
    pub(super) fn dock(
        &mut self,
        ui: &mut Ui,
        sharing: bool,
        sound: Option<&SoundSupport>,
        clicked: &mut Clicked,
    ) {
        if self.step == Step::SelectArea {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(self.area_hint()).color(MUTED));
            });
            ui.add_space(4.0);
        }
        let step_button = self.step != Step::Desktop;
        let buttons = if step_button { 4 } else { 3 };
        let width = dock_width(buttons);
        let row = vec2(ui.available_width(), BUTTON_H);
        ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            if button(ui, "Cancel", Tone::Quiet).clicked() {
                clicked.close = true;
            }
            if step_button {
                self.step_button(ui, clicked);
            }
            self.sound(ui, sound);
            divider(ui);
            ui.add_enabled_ui(self.selected.is_some(), |ui| {
                let label = if sharing { "Switch" } else { "Share" };
                let share = pill_sized(
                    ui,
                    Some(Icon::Share),
                    label,
                    Tone::Primary,
                    BUTTON_W,
                    BUTTON_H,
                );
                clicked.share |= share.clicked();
            });
        });
    }

    /// Select area on the grid; Back or Choose again while selecting an area.
    fn step_button(&self, ui: &mut Ui, clicked: &mut Clicked) {
        match self.step {
            Step::Choose => {
                let label = if self.crop.is_some() {
                    "Change area"
                } else {
                    "Select area"
                };
                let hint = "Share only part of this. It stays the same when you switch to \
                            another app.";
                ui.add_enabled_ui(self.selected.is_some(), |ui| {
                    if button(ui, label, Tone::Quiet).on_hover_text(hint).clicked() {
                        clicked.step = Some(Step::SelectArea);
                    }
                });
            }
            Step::SelectArea if screen::uses_system_chooser() => {
                if button(ui, "Choose again", Tone::Quiet).clicked() {
                    clicked.step = Some(Step::Desktop);
                }
            }
            Step::SelectArea => {
                if button(ui, "Back", Tone::Quiet).clicked() {
                    clicked.step = Some(Step::Choose);
                }
            }
            Step::Desktop => {}
        }
    }

    /// The computer sound switch, greyed out with the reason where it cannot work.
    fn sound(&mut self, ui: &mut Ui, support: Option<&SoundSupport>) {
        let why = match support {
            Some(SoundSupport::Ready) => None,
            Some(SoundSupport::Unavailable(why)) => Some(*why),
            None => Some("Checking…"),
        };
        if why.is_some() {
            self.with_sound = false;
        }
        let response = ui
            .add_enabled_ui(why.is_none(), |ui| sound_switch(ui, self.with_sound))
            .inner
            .on_hover_text(
                "Include what your computer plays, such as a video. Call sound is left out, so \
                 nobody hears an echo.",
            )
            .on_disabled_hover_text(why.unwrap_or("Sharing sound is not available here."));
        if response.clicked() {
            self.with_sound = !self.with_sound;
        }
    }
}

fn button(ui: &mut Ui, label: &str, tone: Tone) -> Response {
    pill_sized(ui, None, label, tone, BUTTON_W, BUTTON_H)
}

/// A dock button with the sound icon, filled in the accent colour while on. The caller flips
/// the choice on click.
fn sound_switch(ui: &mut Ui, on: bool) -> Response {
    const ICON: f32 = 16.0;
    let label = "Sound";
    let (rect, response) = ui.allocate_exact_size(vec2(BUTTON_W, BUTTON_H), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, on, "Computer sound"));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let fill = match (enabled && on, response.hovered()) {
        (true, true) => theme::ACCENT_HOVER,
        (true, false) => ACCENT,
        (false, true) if enabled => RAISED_HOVER,
        _ => RAISED,
    };
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            theme::RADIUS + 2,
            Stroke::new(2.0, TEXT),
            StrokeKind::Outside,
        );
    }
    let colour = match (enabled, on) {
        (false, _) => MUTED,
        (true, true) => Color32::WHITE,
        (true, false) => TEXT,
    };
    let galley = painter.layout_no_wrap(label.to_owned(), theme::bold(14.0), colour);
    let left = rect.center().x - (ICON + GAP + galley.size().x) / 2.0;
    Icon::Volume.paint(
        painter,
        pos2(left + ICON / 2.0, rect.center().y),
        ICON,
        colour,
    );
    let at = pos2(left + ICON + GAP, rect.center().y - galley.size().y / 2.0);
    painter.galley(at, galley, colour);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The thin upright line before Share.
fn divider(ui: &mut Ui) {
    ui.add_space(DIVIDER_PAD);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 28.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::LINE);
    ui.add_space(DIVIDER_PAD);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dock_is_as_wide_as_its_buttons_gaps_and_divider() {
        let divider = 2.0 * (GAP + DIVIDER_PAD) + 1.0;
        assert_eq!(dock_width(4), 4.0 * BUTTON_W + 2.0 * GAP + divider);
        assert_eq!(dock_width(3), 3.0 * BUTTON_W + GAP + divider);
    }

    #[test]
    fn the_widest_dock_fits_the_narrowest_picker() {
        // The picker is never narrower than 540 points.
        assert!(dock_width(4) <= 540.0);
    }
}
