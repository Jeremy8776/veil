//! The share picker's live thumbnails: the capture thread behind them, and shrinking pictures.
//!
//! Purpose: keeps the list of sources current and refreshes the thumbnails you can see.
//!
//! Invariants:
//! - Thumbnails stay on this device.
//! - The first pass fills both tabs. After that only the open tab is captured.
//! - The rest between passes follows what a pass cost, so capture takes about a quarter of one
//!   processor core at most. A system with slow screenshots simply refreshes less often.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use veil_media::screen::{self, Grabbed, ScreenSource};

use super::{CANNOT_CAPTURE, State, wait};
use crate::convert::Rgba;
use crate::media::lock;

/// The largest size of a thumbnail, in pixels.
const THUMBNAIL_BOX: (u32, u32) = (512, 288);
/// Time spent resting for each unit of time spent capturing.
const REST_PER_WORK: u32 = 3;
const MIN_REST: Duration = Duration::from_millis(150);
const MAX_REST: Duration = Duration::from_secs(2);

fn rest_after(pass: Duration) -> Duration {
    (pass * REST_PER_WORK).clamp(MIN_REST, MAX_REST)
}

fn on_open_tab(source: ScreenSource, windows_tab: bool) -> bool {
    matches!(source, ScreenSource::Window(_)) == windows_tab
}

pub(super) fn browse_loop(state: &State, ctx: &egui::Context, stop: &AtomicBool) {
    if let Some(reason) = screen::blocked_reason() {
        *lock(&state.error) = Some(reason.to_string());
        lock(&state.choices).get_or_insert_with(Vec::new);
        ctx.request_repaint();
        return;
    }
    let mut first_pass = true;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let rest = match screen::open_all(std::process::id()) {
            Ok(sources) => {
                let list: Vec<_> = sources.iter().map(|(choice, _)| choice.clone()).collect();
                let mut choices = lock(&state.choices);
                if choices.as_ref() != Some(&list) {
                    *choices = Some(list);
                    ctx.request_repaint();
                }
                drop(choices);
                for (choice, capture) in &sources {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let windows_tab = state.windows_tab.load(Ordering::Acquire);
                    if !first_pass && !on_open_tab(choice.source, windows_tab) {
                        continue;
                    }
                    if let Ok(Some(picture)) = capture.grab() {
                        let small = shrink(&picture, THUMBNAIL_BOX);
                        lock(&state.thumbnails).insert(choice.source, small);
                        ctx.request_repaint();
                    }
                }
                first_pass = false;
                rest_after(started.elapsed())
            }
            Err(e) => {
                crate::note!("screen share: cannot list sources: {e}");
                *lock(&state.error) = Some(CANNOT_CAPTURE.to_string());
                lock(&state.choices).get_or_insert_with(Vec::new);
                ctx.request_repaint();
                MAX_REST
            }
        };
        wait(rest, stop);
    }
}

/// Shrinks to fit inside `(max_width, max_height)`. Each new pixel is the average of the block
/// it replaces, so text stays legible. The result is opaque.
pub(super) fn shrink(picture: &Grabbed, (max_width, max_height): (u32, u32)) -> Rgba {
    let across = picture.width.div_ceil(max_width.max(1));
    let down = picture.height.div_ceil(max_height.max(1));
    let step = across.max(down).max(1) as usize;
    let (width, height) = (picture.width as usize, picture.height as usize);
    let (out_w, out_h) = ((width / step).max(1), (height / step).max(1));
    let mut pixels = Vec::with_capacity(out_w * out_h * 4);
    let mut sums = vec![[0u32; 3]; out_w];
    for out_y in 0..out_h {
        sums.fill([0; 3]);
        let rows = out_y * step..((out_y + 1) * step).min(height);
        let block = (rows.len() * step.min(width)).max(1) as u32;
        for y in rows {
            let row = &picture.rgba[y * width * 4..][..width * 4];
            for (sum, part) in sums.iter_mut().zip(row.chunks(step * 4)) {
                for pixel in part.as_chunks::<4>().0 {
                    sum[0] += u32::from(pixel[0]);
                    sum[1] += u32::from(pixel[1]);
                    sum[2] += u32::from(pixel[2]);
                }
            }
        }
        for sum in &sums {
            pixels.extend(sum.map(|channel| (channel / block) as u8));
            pixels.push(u8::MAX);
        }
    }
    Rgba {
        width: out_w,
        height: out_h,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_follows_the_cost_of_a_pass_within_limits() {
        assert_eq!(rest_after(Duration::from_millis(1)), MIN_REST);
        assert_eq!(rest_after(Duration::from_millis(100)), Duration::from_millis(300));
        assert_eq!(rest_after(Duration::from_secs(5)), MAX_REST);
    }

    #[test]
    fn only_the_open_tab_counts() {
        assert!(on_open_tab(ScreenSource::Monitor(1), false));
        assert!(!on_open_tab(ScreenSource::Window(1), false));
        assert!(on_open_tab(ScreenSource::Window(1), true));
        assert!(!on_open_tab(ScreenSource::Monitor(1), true));
    }

    #[test]
    fn a_large_picture_is_shrunk_and_a_small_one_kept() {
        let picture = Grabbed {
            width: 8,
            height: 4,
            rgba: vec![7; 8 * 4 * 4],
        };
        let small = shrink(&picture, (4, 4));
        assert_eq!((small.width, small.height), (4, 2));
        assert_eq!(small.pixels.len(), 4 * 2 * 4);
        let same = shrink(&picture, (100, 100));
        assert_eq!((same.width, same.height), (8, 4));
    }

    #[test]
    fn a_tall_picture_fits_the_height_too() {
        let picture = Grabbed {
            width: 4,
            height: 16,
            rgba: vec![7; 4 * 16 * 4],
        };
        let small = shrink(&picture, (4, 4));
        assert_eq!((small.width, small.height), (1, 4));
    }

    #[test]
    fn shrinking_averages_and_makes_pixels_opaque() {
        let picture = Grabbed {
            width: 2,
            height: 2,
            rgba: vec![0, 0, 0, 0, 200, 100, 40, 0, 0, 0, 0, 0, 200, 100, 40, 0],
        };
        let small = shrink(&picture, (1, 1));
        assert_eq!(small.pixels, [100, 50, 20, 255]);
    }
}
