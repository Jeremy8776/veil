//! Veil's mark, drawn live on the home screen: a teal disc with the V cut out, and waves that
//! ripple out from it.
//!
//! Purpose: the same mark as the app icon (`tools/make-icons.py`, whose geometry this copies),
//! with the two static rings replaced by slow waves.
//!
//! Invariants:
//! - The V is painted in the colour behind the logo, so the logo must sit on a plain fill of
//!   that colour (the home page). That reads as a cut-out without a stencil.
//! - Motion is slow and quiet (one wave every [`PERIOD`] / [`WAVES`] seconds), and the logo
//!   asks for about 30 frames a second only while it is on screen.

use std::f32::consts::PI;
use std::time::Duration;

use eframe::egui::{self, Color32, Mesh, Pos2, Sense, Shape, Stroke, pos2, vec2};

use crate::theme::{LOGO_DEEP, LOGO_LIGHT};

/// Seconds for one wave to travel from the disc to the edge.
const PERIOD: f32 = 3.6;
/// Waves in flight at once, evenly spaced in time.
const WAVES: usize = 3;
/// Geometry in units of the half-width, as in `tools/make-icons.py`.
const DISC: f32 = 0.533;
const RING_WIDTH: f32 = 0.109;
const V: [(f32, f32); 3] = [(-0.207, -0.152), (0.0, 0.207), (0.207, -0.152)];
const V_WIDTH: f32 = 0.185;
/// Where a wave's centre line starts and ends.
const WAVE_FROM: f32 = DISC + RING_WIDTH * 0.5;
const WAVE_TO: f32 = 1.0 - RING_WIDTH * 0.5;
/// A wave's strongest opacity.
const WAVE_ALPHA: f32 = 0.65;

/// Draws the logo `size` points square, centred in the row, on a `behind` coloured page.
pub fn show(ui: &mut egui::Ui, size: f32, behind: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let time = ui.input(|i| i.time) as f32;
    let painter = ui.painter_at(rect.expand(1.0));
    let centre = rect.center();
    let unit = size / 2.0;
    for (radius, alpha) in waves(time) {
        let colour = gradient(0.0, 0.0).gamma_multiply(alpha);
        painter.circle_stroke(
            centre,
            radius * unit,
            Stroke::new(RING_WIDTH * unit, colour),
        );
    }
    painter.add(disc(centre, unit));
    let points: Vec<Pos2> = V.iter().map(|&(x, y)| centre + vec2(x, y) * unit).collect();
    let width = V_WIDTH * unit;
    painter.add(Shape::line(points.clone(), Stroke::new(width, behind)));
    for point in points {
        // Round caps and a round join, as in the icon.
        painter.circle_filled(point, width / 2.0, behind);
    }
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

/// Each wave's radius and opacity at `time` seconds. A wave fades in just off the disc,
/// spreads out, and is gone by the edge, so a new one starts with no jump.
fn waves(time: f32) -> [(f32, f32); WAVES] {
    std::array::from_fn(|i| {
        let phase = (time / PERIOD + i as f32 / WAVES as f32).rem_euclid(1.0);
        let radius = WAVE_FROM + (WAVE_TO - WAVE_FROM) * phase;
        // Zero at both ends, strongest a quarter of the way out.
        let alpha = WAVE_ALPHA * (PI * phase.powf(0.5)).sin();
        (radius, alpha.max(0.0))
    })
}

/// The teal at a point (in half-width units from the centre): light at the top left, deep at
/// the bottom right, the same direction as the icon.
fn gradient(x: f32, y: f32) -> Color32 {
    let t = ((0.3 * x + y) / 1.3 + 1.0) / 2.0;
    lerp(LOGO_LIGHT, LOGO_DEEP, t.clamp(0.0, 1.0))
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// The disc as a triangle fan, coloured per vertex so it carries the gradient.
fn disc(centre: Pos2, unit: f32) -> Shape {
    const SEGMENTS: u32 = 72;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(centre, gradient(0.0, 0.0));
    for i in 0..SEGMENTS {
        let angle = i as f32 / SEGMENTS as f32 * 2.0 * PI;
        let (x, y) = (angle.cos() * DISC, angle.sin() * DISC);
        mesh.colored_vertex(
            pos2(centre.x + x * unit, centre.y + y * unit),
            gradient(x, y),
        );
    }
    for i in 0..SEGMENTS {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % SEGMENTS);
    }
    Shape::mesh(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waves_stay_between_the_disc_and_the_edge() {
        for step in 0..400 {
            for (radius, alpha) in waves(step as f32 * 0.05) {
                assert!((WAVE_FROM..=WAVE_TO).contains(&radius), "radius {radius}");
                assert!((0.0..=WAVE_ALPHA).contains(&alpha), "alpha {alpha}");
            }
        }
    }

    #[test]
    fn a_wave_is_invisible_where_it_starts_and_ends() {
        // At phase 0 a wave sits on the disc; just before phase 1 it is at the edge.
        let (_, at_start) = waves(0.0)[0];
        let (_, near_end) = waves(PERIOD * 0.999)[0];
        assert!(at_start < 0.01 && near_end < 0.05, "{at_start} {near_end}");
    }

    #[test]
    fn the_animation_repeats_every_period() {
        let a = waves(1.3);
        let b = waves(1.3 + PERIOD);
        for ((ra, aa), (rb, ab)) in a.iter().zip(b.iter()) {
            assert!((ra - rb).abs() < 1e-3 && (aa - ab).abs() < 1e-3);
        }
    }

    #[test]
    fn the_gradient_runs_light_to_deep() {
        assert_eq!(gradient(-1.0, -1.0), LOGO_LIGHT);
        assert_eq!(gradient(1.0, 1.0), LOGO_DEEP);
    }
}
