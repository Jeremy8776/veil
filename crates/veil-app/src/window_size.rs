//! The window's opening size: the same share of the screen on every monitor.
//!
//! Purpose: on a 2560 x 1440 screen at 100% scaling, Veil opens 1613 x 1045 (the size chosen
//! by hand on 26/11/2026). Other screens get the same fraction of their width and height, in
//! points, so a 4K screen at 150% (also 2560 x 1440 points) opens the same, and a 1080p
//! screen opens smaller.
//!
//! Invariants:
//! - The monitor size is only known once the window is open, so the window opens at the
//!   builder's fallback size and is resized and centred on its first frames. It is done once
//!   per run; the person's own resizing is never undone.
//! - Never smaller than the window's minimum, never larger than the monitor.

use eframe::egui::{self, Vec2, vec2};

/// The fraction of the monitor's width and height, from 1613 x 1045 on 2560 x 1440.
const SHARE: Vec2 = vec2(1613.0 / 2560.0, 1045.0 / 1440.0);
/// The smallest the window may be (`lib.rs` sets the same on the builder).
pub const MIN: Vec2 = vec2(880.0, 600.0);
/// The size until the monitor is known.
pub const FALLBACK: Vec2 = vec2(1280.0, 800.0);
/// Frames to wait for the monitor size before giving up and keeping the fallback.
const PATIENCE: u32 = 10;

/// The opening size for a monitor `monitor` points across.
pub fn opening(monitor: Vec2) -> Vec2 {
    (monitor * SHARE).round().max(MIN).min(monitor)
}

#[derive(Clone, Copy, Default)]
enum Step {
    #[default]
    Waiting,
    Resized,
    Done,
}

/// Called each frame on desktop: sizes the window on its first frame with a monitor size,
/// then centres it on the next (centring uses the size it has).
pub fn apply_once(ctx: &egui::Context) {
    let id = egui::Id::unique("veil-opening-size");
    let (step, frames) = ctx
        .data(|d| d.get_temp::<(Step, u32)>(id))
        .unwrap_or_default();
    let next = match step {
        Step::Done => return,
        Step::Waiting => match ctx.input(|i| i.viewport().monitor_size) {
            Some(monitor) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(opening(monitor)));
                Step::Resized
            }
            None if frames >= PATIENCE => Step::Done,
            None => Step::Waiting,
        },
        Step::Resized => {
            if let Some(centre) = egui::ViewportCommand::center_on_screen(ctx) {
                ctx.send_viewport_cmd(centre);
            }
            Step::Done
        }
    };
    ctx.data_mut(|d| d.insert_temp(id, (next, frames + 1)));
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_1440p_screen_gets_the_chosen_size() {
        assert_eq!(opening(vec2(2560.0, 1440.0)), vec2(1613.0, 1045.0));
    }

    #[test]
    fn other_screens_get_the_same_share() {
        // 4K at 100%: 1.5 times the 1440p size, give or take rounding.
        let uhd = opening(vec2(3840.0, 2160.0));
        assert!(
            (uhd - vec2(1613.0, 1045.0) * 1.5).length() <= 1.0,
            "{uhd:?}"
        );
        let hd = opening(vec2(1920.0, 1080.0));
        assert_eq!(hd, vec2(1210.0, 784.0));
    }

    #[test]
    fn a_small_screen_keeps_the_minimum_but_never_exceeds_the_screen() {
        assert_eq!(opening(vec2(1280.0, 720.0)), vec2(880.0, 600.0));
        assert_eq!(opening(vec2(800.0, 500.0)), vec2(800.0, 500.0));
    }
}
