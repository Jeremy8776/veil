//! Veil's mark, drawn live on the home screen: a white disc with the V cut out, and waves that
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

use eframe::egui::{self, Color32, Pos2, Sense, Shape, Stroke, vec2};

use crate::theme::LOGO;

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
pub fn mark(painter: &egui::Painter, rect: egui::Rect, behind: Color32) {
    let centre = rect.center();
    let unit = rect.width().min(rect.height()) / 2.0;
    for radius in [0.73, 0.94] {
        painter.circle_stroke(centre, radius * unit, Stroke::new(RING_WIDTH * unit, LOGO));
    }
    painter.circle_filled(centre, DISC * unit, LOGO);
    let points: Vec<Pos2> = V.iter().map(|&(x, y)| centre + vec2(x, y) * unit).collect();
    painter.add(Shape::line(points.clone(), Stroke::new(V_WIDTH * unit, behind)));
    for point in points { painter.circle_filled(point, V_WIDTH * unit / 2.0, behind); }
}

/// Draws the animated desktop mark.
pub fn show(ui: &mut egui::Ui, size: f32, behind: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let time = ui.input(|i| i.time) as f32;
    let painter = ui.painter_at(rect.expand(1.0));
    let centre = rect.center();
    let unit = size / 2.0;
    for (radius, alpha) in waves(time) {
        painter.circle_stroke(
            centre,
            radius * unit,
            Stroke::new(RING_WIDTH * unit, LOGO.gamma_multiply(alpha)),
        );
    }
    painter.circle_filled(centre, DISC * unit, LOGO);
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
}
