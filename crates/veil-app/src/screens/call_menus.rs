//! The pieces the call bar's pop-up menus are built from: the menu frame, section headings,
//! choice rows with a tick, the volume slider, the blur switch, the filter swatches and a
//! full-width on/off button.
//!
//! Purpose: the Audio, Video and Effects menus look like Zoom's and Teams' (a list you tick,
//! not a drop-down inside a pop-up).
//!
//! Invariants:
//! - Nothing here opens a pop-up of its own. egui keeps one open pop-up per window
//!   (`Memory::popups`), so a drop-down inside a menu replaces the menu and the choice is
//!   lost. That is why the menus first shipped broken in 0.1.6.
//! - Every control reports a label and its state to screen readers.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Rect, Response, Role, Sense, Shape, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};
use veil_media::{Blur, Filter};

use crate::icons::Icon;
use crate::media::LocalMedia;
use crate::theme::{self, ACCENT, LINE, MUTED, RAISED, RAISED_HOVER, SURFACE, TEXT};

const ROW_H: f32 = 32.0;
/// Device lists longer than this scroll.
const LIST_MAX_H: f32 = 6.0 * ROW_H;
const KNOB: f32 = 7.0;

/// The frame around every call-bar menu.
pub fn frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(110),
        })
}

/// A small muted heading with an icon, at the top of a menu section.
pub fn section(ui: &mut Ui, icon: Icon, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let painter = ui.painter();
    icon.paint(
        painter,
        pos2(rect.left() + 12.0, rect.center().y),
        14.0,
        MUTED,
    );
    painter.text(
        pos2(rect.left() + 26.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::bold(12.0),
        MUTED,
    );
}

/// A thin line between menu sections.
pub fn divider(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, LINE);
    ui.add_space(4.0);
}

/// One choice in a list: a tick when chosen, the name cut short with "…" if it is long
/// (the full name is then in the tooltip). Returns the row's response; it was picked when
/// `clicked()`.
pub fn choice(ui: &mut Ui, label: &str, chosen: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, chosen, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if response.hovered() || response.has_focus() {
        painter.rect_filled(rect, CornerRadius::same(6), RAISED_HOVER);
    }
    if chosen {
        tick(painter, pos2(rect.left() + 13.0, rect.center().y), ACCENT);
    }
    let text_left = rect.left() + 28.0;
    let mut job = egui::text::LayoutJob::single_section(
        label.to_string(),
        egui::TextFormat::simple(FontId::proportional(13.5), TEXT),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - 8.0 - text_left);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let elided = galley.elided;
    painter.galley(
        pos2(text_left, rect.center().y - galley.size().y / 2.0),
        galley,
        TEXT,
    );
    if elided {
        response.on_hover_text(label)
    } else {
        response
    }
}

/// A list of devices with "default" first. Returns the new choice when one was picked
/// (`Some(None)` for the default).
pub fn device_list(
    ui: &mut Ui,
    id: &str,
    list: &[crate::devices::Device],
    chosen: Option<&str>,
    default: &str,
) -> Option<Option<String>> {
    let mut picked = None;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(LIST_MAX_H)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            // A choice that was unplugged shows the default as ticked, as the label does.
            let known = chosen.filter(|c| list.iter().any(|d| d.id == *c));
            if choice(ui, default, known.is_none()).clicked() && chosen.is_some() {
                picked = Some(None);
            }
            for device in list {
                let this = known == Some(device.id.as_str());
                if choice(ui, &device.name, this).clicked() && !this {
                    picked = Some(Some(device.id.clone()));
                }
            }
        });
    picked
}

/// A volume row: speaker icon, slider and the percentage. Returns true when it moved.
pub fn volume(ui: &mut Ui, label: &str, gain: &mut f32, max: f32, hint: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (icon, _) = ui.allocate_exact_size(vec2(18.0, 24.0), Sense::hover());
        Icon::Volume.paint(ui.painter(), icon.center(), 14.0, MUTED);
        let width = ui.available_width() - 44.0;
        changed = slider(ui, gain, max, width, label)
            .on_hover_text(hint)
            .changed();
        let (text, _) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::hover());
        ui.painter().text(
            text.right_center(),
            Align2::RIGHT_CENTER,
            format!("{}%", percent(*gain)),
            FontId::proportional(12.5),
            MUTED,
        );
    });
    changed
}

/// A gain as a whole percentage.
pub fn percent(gain: f32) -> u16 {
    (gain.clamp(0.0, 6.0) * 100.0).round() as u16
}

/// The gain for a point `t` (0 to 1) along a slider that ends at `max`: whole percents, held
/// at 100% when close to it so the normal level is easy to find again.
fn gain_at(t: f32, max: f32) -> f32 {
    let gain = (t.clamp(0.0, 1.0) * max * 100.0).round() / 100.0;
    if max > 1.0 && (gain - 1.0).abs() < 0.04 {
        1.0
    } else {
        gain
    }
}

/// A slim slider: a track filled in the accent colour up to a round knob, with a mark at 100%
/// when the slider goes past it. Drag, click, or use the arrow keys when focused.
fn slider(ui: &mut Ui, gain: &mut f32, max: f32, width: f32, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
    let track = Rect::from_min_max(
        pos2(rect.left() + KNOB, rect.center().y - 2.0),
        pos2(rect.right() - KNOB, rect.center().y + 2.0),
    );
    let old = *gain;
    if let Some(pointer) = response.interact_pointer_pos() {
        *gain = gain_at((pointer.x - track.left()) / track.width(), max);
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            );
        });
        let step = ui.input(|i| {
            i32::from(i.key_pressed(Key::ArrowRight)) - i32::from(i.key_pressed(Key::ArrowLeft))
        });
        if step != 0 {
            *gain = (*gain + step as f32 * 0.05).clamp(0.0, max);
        }
    }
    if (*gain - old).abs() > f32::EPSILON {
        response.mark_changed();
    }
    let value = f64::from(percent(*gain));
    response.widget_info(|| WidgetInfo::slider(true, value, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let x = track.left() + track.width() * (*gain / max).clamp(0.0, 1.0);
    painter.rect_filled(track, CornerRadius::same(2), LINE);
    let filled = Rect::from_min_max(track.min, pos2(x, track.max.y));
    painter.rect_filled(filled, CornerRadius::same(2), ACCENT);
    if max > 1.0 {
        let normal = track.left() + track.width() / max;
        painter.line_segment(
            [
                pos2(normal, track.top() - 3.0),
                pos2(normal, track.bottom() + 3.0),
            ],
            Stroke::new(1.5, MUTED),
        );
    }
    let active = response.hovered() || response.dragged();
    let knob = pos2(x, rect.center().y);
    painter.circle_filled(knob, if active { KNOB + 1.0 } else { KNOB }, TEXT);
    if response.has_focus() {
        painter.circle_stroke(knob, KNOB + 3.0, Stroke::new(2.0, ACCENT));
    }
    response
}

/// Off, Light and Strong side by side; the chosen one filled. Sets blur when another is
/// clicked, and shows the notice if blur turned itself off.
pub fn blur_switch(ui: &mut Ui, media: &LocalMedia) {
    let current = media.blur();
    let (rect, frame) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    frame.on_hover_text(
        "Blur is not a privacy guarantee: edges and hair can show what is behind you.",
    );
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), RAISED);
    let w = (rect.width() - 6.0) / Blur::ALL.len() as f32;
    for (i, blur) in Blur::ALL.into_iter().enumerate() {
        let part = Rect::from_min_size(
            pos2(rect.left() + 3.0 + i as f32 * w, rect.top() + 3.0),
            vec2(w, rect.height() - 6.0),
        );
        let label = blur_label(blur);
        let chosen = blur == current;
        let response = ui.interact(part, ui.scope_id().with(("blur", i)), Sense::click());
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, chosen, label));
        let painter = ui.painter();
        let fill = if chosen {
            ACCENT
        } else if response.hovered() {
            RAISED_HOVER
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(part, CornerRadius::same(6), fill);
        if response.has_focus() {
            painter.rect_stroke(part, 6, Stroke::new(2.0, TEXT), StrokeKind::Inside);
        }
        painter.text(
            part.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13.0),
            if chosen { Color32::WHITE } else { TEXT },
        );
        if response.clicked() && !chosen {
            media.set_blur(blur);
        }
    }
    if let Some(notice) = media.blur_notice() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(notice)
                .small()
                .color(theme::DANGER_HOVER),
        )
        .on_hover_text("Turn the camera on again when fewer apps are running, or turn blur off.");
    }
}

/// A full-width button with an icon, filled in the accent colour while `on`. Sized and coloured
/// like the blur switch above it.
pub fn toggle_button(ui: &mut Ui, icon: Icon, label: &str, on: bool) -> Response {
    const ICON: f32 = 16.0;
    const GAP: f32 = 8.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let fill = if on {
        ACCENT
    } else if response.hovered() {
        RAISED_HOVER
    } else {
        RAISED
    };
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    if response.has_focus() {
        painter.rect_stroke(rect, 8, Stroke::new(2.0, TEXT), StrokeKind::Inside);
    }
    let colour = if on { Color32::WHITE } else { TEXT };
    let galley = painter.layout_no_wrap(label.to_string(), FontId::proportional(13.0), colour);
    let left = rect.center().x - (ICON + GAP + galley.size().x) / 2.0;
    let y = rect.center().y;
    icon.paint(painter, pos2(left + ICON / 2.0, y), ICON, colour);
    let text_at = pos2(left + ICON + GAP, y - galley.size().y / 2.0);
    painter.galley(text_at, galley, colour);
    response
}

fn blur_label(blur: Blur) -> &'static str {
    match blur {
        Blur::Off => "Off",
        Blur::Light => "Light",
        Blur::Strong => "Strong",
    }
}

/// One round swatch per filter, its short name below; the chosen one ringed in the accent.
pub fn filter_swatches(ui: &mut Ui, media: &LocalMedia) {
    let current = media.filter();
    let count = Filter::ALL.len() as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::hover());
    let w = rect.width() / count;
    for (i, filter) in Filter::ALL.into_iter().enumerate() {
        let cell = Rect::from_min_size(pos2(rect.left() + i as f32 * w, rect.top()), vec2(w, 62.0));
        let (short, full) = filter_label(filter);
        let chosen = filter == current;
        let response = ui
            .interact(cell, ui.scope_id().with(("filter", i)), Sense::click())
            .on_hover_text(full);
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, chosen, full));
        let painter = ui.painter();
        if response.hovered() || response.has_focus() {
            painter.rect_filled(cell.shrink(2.0), CornerRadius::same(8), RAISED_HOVER);
        }
        let centre = pos2(cell.center().x, cell.top() + 22.0);
        swatch(painter, centre, 14.0, filter);
        if chosen {
            painter.circle_stroke(centre, 17.5, Stroke::new(2.0, ACCENT));
        }
        painter.text(
            pos2(cell.center().x, cell.bottom() - 11.0),
            Align2::CENTER_CENTER,
            short,
            FontId::proportional(11.5),
            if chosen { TEXT } else { MUTED },
        );
        if response.clicked() && !chosen {
            media.set_filter(filter);
        }
    }
}

/// A short name for the swatch and the full one for tooltips and screen readers.
fn filter_label(filter: Filter) -> (&'static str, &'static str) {
    match filter {
        Filter::None => ("None", "No filter"),
        Filter::Warm => ("Warm", "Warm"),
        Filter::Cool => ("Cool", "Cool"),
        Filter::BlackWhite => ("B&W", "Black & white"),
        Filter::Bright => ("Bright", "Bright"),
    }
}

/// A small picture of what a filter does to colour.
fn swatch(painter: &egui::Painter, c: egui::Pos2, r: f32, filter: Filter) {
    match filter {
        Filter::None => {
            painter.circle_filled(c, r, RAISED);
            painter.circle_stroke(c, r, Stroke::new(1.0, LINE));
            let d = r * 0.6;
            painter.line_segment(
                [pos2(c.x - d, c.y + d), pos2(c.x + d, c.y - d)],
                Stroke::new(1.5, MUTED),
            );
        }
        Filter::BlackWhite => {
            painter.circle_filled(c, r, Color32::from_gray(235));
            // The left half dark: a half circle as a fan of points.
            let half: Vec<_> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::FRAC_PI_2 + i as f32 / 16.0 * std::f32::consts::PI;
                    pos2(c.x + r * a.cos(), c.y - r * a.sin())
                })
                .collect();
            painter.add(Shape::convex_polygon(
                half,
                Color32::from_gray(40),
                Stroke::NONE,
            ));
        }
        Filter::Warm => {
            painter.circle_filled(c, r, Color32::from_rgb(0xE8, 0x9A, 0x52));
        }
        Filter::Cool => {
            painter.circle_filled(c, r, Color32::from_rgb(0x57, 0x9C, 0xE6));
        }
        Filter::Bright => {
            painter.circle_filled(c, r, Color32::from_rgb(0xF5, 0xE6, 0xB4));
        }
    }
}

/// A tick mark, for the chosen row.
fn tick(painter: &egui::Painter, c: egui::Pos2, colour: Color32) {
    let stroke = Stroke::new(2.0, colour);
    painter.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x - 1.5, c.y + 3.5)], stroke);
    painter.line_segment(
        [pos2(c.x - 1.5, c.y + 3.5), pos2(c.x + 5.0, c.y - 4.0)],
        stroke,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_show_as_whole_percentages() {
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(2.0), 200);
        assert_eq!(percent(0.333), 33);
        assert_eq!(percent(-1.0), 0);
    }

    #[test]
    fn slider_ends_and_whole_percents() {
        assert_eq!(gain_at(0.0, 2.0), 0.0);
        assert_eq!(gain_at(1.0, 2.0), 2.0);
        assert_eq!(gain_at(-0.5, 2.0), 0.0);
        assert_eq!(gain_at(0.256, 2.0), 0.51);
    }

    #[test]
    fn slider_holds_at_the_normal_level() {
        assert_eq!(gain_at(0.51, 2.0), 1.0);
        assert_eq!(gain_at(0.49, 2.0), 1.0);
        assert_eq!(gain_at(0.6, 2.0), 1.2);
        // A slider that ends at 100% has no hold.
        assert_eq!(gain_at(0.98, 1.0), 0.98);
    }
}
