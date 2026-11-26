//! The look grid in the Effects menu: one round swatch per built-in Filter and per imported
//! LUT, five to a row, the short name under each.
//!
//! Purpose: a LUT reads at a glance like the built-in filters. Its swatch is its tone ramp
//! (`luts::Ramp`): what it does to greys from shadow (bottom) to highlight (top), so split
//! toning, lifted blacks and black and white each look different.
//!
//! Invariants:
//! - Nothing here opens a pop-up (see `call_menus.rs`).
//! - Each swatch reports its full name and whether it is chosen to screen readers. A LUT's
//!   remove button appears while the pointer is over its cell, and is a labelled button too.
//! - Long names are cut to one line with an ellipsis; the full name is in the tooltip.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Role, Sense, Shape, Stroke, Ui,
    WidgetInfo, epaint::Vertex, pos2, text::LayoutJob, vec2,
};
use veil_media::Filter;

use crate::icons::Icon;
use crate::luts::Ramp;
use crate::media::LocalMedia;
use crate::theme::{ACCENT, LINE, MUTED, RAISED, RAISED_HOVER, TEXT};

const COLUMNS: usize = 5;
const CELL_H: f32 = 62.0;
const RADIUS: f32 = 14.0;
/// The remove button in a LUT cell's top right corner.
const REMOVE: f32 = 16.0;

/// What a swatch shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Filter(Filter),
    Ramp(Ramp),
    /// A LUT whose file no longer loads.
    Unreadable,
}

/// One cell of the grid.
pub struct Swatch {
    pub short: String,
    pub full: String,
    pub chosen: bool,
    pub paint: Paint,
    /// An imported LUT, which can be removed here.
    pub removable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Choose(usize),
    Remove(usize),
}

/// The built-in Filters as swatches. None is chosen while a LUT is in use.
pub fn filters(media: &LocalMedia) -> Vec<Swatch> {
    let lut_on = media.lut().is_some();
    let current = media.filter();
    Filter::ALL
        .into_iter()
        .map(|filter| {
            let (short, full) = filter_label(filter);
            Swatch {
                short: short.into(),
                full: full.into(),
                chosen: !lut_on && filter == current,
                paint: Paint::Filter(filter),
                removable: false,
            }
        })
        .collect()
}

/// The grid. Returns what was clicked, by index into `cells`.
pub fn grid(ui: &mut Ui, cells: &[Swatch]) -> Option<Action> {
    let rows = cells.len().div_ceil(COLUMNS);
    let size = vec2(ui.available_width(), rows as f32 * CELL_H);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let w = rect.width() / COLUMNS as f32;
    let mut action = None;
    for (i, cell) in cells.iter().enumerate() {
        let at = pos2(
            rect.left() + (i % COLUMNS) as f32 * w,
            rect.top() + (i / COLUMNS) as f32 * CELL_H,
        );
        let area = Rect::from_min_size(at, vec2(w, CELL_H));
        if let Some(a) = one(ui, area, i, cell) {
            action = Some(a);
        }
    }
    action
}

fn one(ui: &mut Ui, area: Rect, i: usize, cell: &Swatch) -> Option<Action> {
    let id = ui.scope_id().with(("swatch", i));
    let response = ui
        .interact(area, id, Sense::click())
        .on_hover_text(&cell.full);
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, cell.chosen, &cell.full));
    let over = ui.rect_contains_pointer(area);
    let painter = ui.painter();
    if over || response.has_focus() {
        painter.rect_filled(area.shrink(2.0), CornerRadius::same(8), RAISED_HOVER);
    }
    let centre = pos2(area.center().x, area.top() + 22.0);
    match cell.paint {
        Paint::Filter(filter) => filter_swatch(painter, centre, RADIUS, filter),
        Paint::Ramp(ramp) => ramp_swatch(painter, centre, RADIUS, &ramp),
        Paint::Unreadable => {
            painter.circle_stroke(centre, RADIUS, Stroke::new(1.0, LINE));
            painter.text(
                centre,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(13.0),
                MUTED,
            );
        }
    }
    if cell.chosen {
        painter.circle_stroke(centre, RADIUS + 3.5, Stroke::new(2.0, ACCENT));
    }
    let colour = if cell.chosen { TEXT } else { MUTED };
    let label = painter.layout_job(one_line(&cell.short, area.width() - 6.0, colour));
    let label_at = pos2(area.center().x, area.bottom() - 11.0) - label.size() / 2.0;
    painter.galley(label_at, label, colour);
    let mut action = (response.clicked() && !cell.chosen).then_some(Action::Choose(i));
    if cell.removable && over {
        let x = Rect::from_min_size(
            pos2(area.right() - REMOVE - 2.0, area.top() + 2.0),
            vec2(REMOVE, REMOVE),
        );
        let remove = ui
            .interact(x, id.with("remove"), Sense::click())
            .on_hover_text(format!("Remove {}", cell.full));
        let name = format!("Remove {}", cell.full);
        remove.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
        let colour = if remove.hovered() { TEXT } else { MUTED };
        Icon::Close.paint(ui.painter(), x.center(), 10.0, colour);
        if remove.clicked() {
            action = Some(Action::Remove(i));
        }
    }
    action
}

/// `text` on one line at most `width` wide, cut with an ellipsis.
fn one_line(text: &str, width: f32, colour: Color32) -> LayoutJob {
    let mut job = LayoutJob::simple_singleline(text.into(), FontId::proportional(11.5), colour);
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.halign = egui::Align::LEFT;
    job
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

/// A small picture of what a built-in filter does to colour.
fn filter_swatch(painter: &egui::Painter, c: Pos2, r: f32, filter: Filter) {
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

/// A LUT's swatch: its ramp as a gradient, shadows at the bottom, highlights at the top.
fn ramp_swatch(painter: &egui::Painter, c: Pos2, r: f32, ramp: &Ramp) {
    painter.add(Shape::mesh(ramp_mesh(c, r, ramp)));
    painter.circle_stroke(c, r, Stroke::new(1.0, Color32::from_white_alpha(28)));
}

/// Points around the rim of a ramp swatch.
const RIM: usize = 48;

/// A disc as a fan from its centre, each point coloured by its height in the ramp.
fn ramp_mesh(c: Pos2, r: f32, ramp: &Ramp) -> Mesh {
    let colour = |p: Pos2| ramp_colour(ramp, 0.5 - (p.y - c.y) / (2.0 * r));
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex {
        pos: c,
        uv: egui::epaint::WHITE_UV,
        color: colour(c),
    });
    for i in 0..RIM {
        let a = i as f32 / RIM as f32 * std::f32::consts::TAU;
        let p = c + vec2(a.cos(), a.sin()) * r;
        mesh.vertices.push(Vertex {
            pos: p,
            uv: egui::epaint::WHITE_UV,
            color: colour(p),
        });
    }
    for i in 0..RIM as u32 {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % RIM as u32);
    }
    mesh
}

/// The ramp's colour at height `t`: 0 is its first (darkest) entry, 1 its last.
fn ramp_colour(ramp: &Ramp, t: f32) -> Color32 {
    let last = (ramp.len() - 1) as f32;
    let x = t.clamp(0.0, 1.0) * last;
    let i = (x.floor() as usize).min(ramp.len() - 2);
    let f = x - i as f32;
    let [r, g, b]: [u8; 3] = std::array::from_fn(|k| {
        ((ramp[i][k] + (ramp[i + 1][k] - ramp[i][k]) * f) * 255.0).round() as u8
    });
    Color32::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREYS: Ramp = [[0.1; 3], [0.3; 3], [0.5; 3], [0.7; 3], [0.9; 3]];

    #[test]
    fn the_ramp_runs_from_shadow_to_highlight() {
        assert_eq!(ramp_colour(&GREYS, 0.0), Color32::from_gray(26));
        assert_eq!(ramp_colour(&GREYS, 0.5), Color32::from_gray(128));
        assert_eq!(ramp_colour(&GREYS, 1.0), Color32::from_gray(230));
        assert_eq!(ramp_colour(&GREYS, 2.0), Color32::from_gray(230), "clamped");
    }

    #[test]
    fn a_swatch_is_dark_at_the_bottom_and_light_at_the_top() {
        let c = pos2(50.0, 50.0);
        let mesh = ramp_mesh(c, 10.0, &GREYS);
        let top = mesh
            .vertices
            .iter()
            .min_by(|a, b| a.pos.y.total_cmp(&b.pos.y));
        let bottom = mesh
            .vertices
            .iter()
            .max_by(|a, b| a.pos.y.total_cmp(&b.pos.y));
        assert_eq!(top.map(|v| v.color), Some(Color32::from_gray(230)));
        assert_eq!(bottom.map(|v| v.color), Some(Color32::from_gray(26)));
        assert_eq!(mesh.indices.len(), RIM * 3);
        assert!(
            mesh.vertices
                .iter()
                .all(|v| (v.pos - c).length() <= 10.0 + 1e-3)
        );
    }
}
