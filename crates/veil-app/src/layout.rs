//! Where video tiles go: the gallery grid and the speaker view's filmstrip.
//!
//! Purpose: pure geometry, so it is tested without a window. The gallery tries every column
//! count and keeps the one with the largest 16:9 tiles; a short last row is centred.
//!
//! Invariants:
//! - Tiles never overlap and stay inside the area given.
//! - Every tile is 16:9 and the same size.

use eframe::egui::{Rect, Vec2, pos2, vec2};

pub const ASPECT: f32 = 16.0 / 9.0;

/// Rectangles for `count` gallery tiles in `area`, `gap` apart, in reading order.
pub fn gallery(count: usize, area: Rect, gap: f32) -> Vec<Rect> {
    if count == 0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return Vec::new();
    }
    let mut best = (1, vec2(0.0, 0.0));
    for cols in 1..=count {
        let rows = count.div_ceil(cols);
        let size = tile_size(cols, rows, area.size(), gap);
        if size.x > best.1.x {
            best = (cols, size);
        }
    }
    let (cols, size) = best;
    let rows = count.div_ceil(cols);
    let used_h = rows as f32 * size.y + (rows - 1) as f32 * gap;
    let top = area.top() + (area.height() - used_h) / 2.0;
    (0..count)
        .map(|i| {
            let (row, col) = (i / cols, i % cols);
            let in_row = (count - row * cols).min(cols);
            let row_w = in_row as f32 * size.x + (in_row - 1) as f32 * gap;
            let left = area.left() + (area.width() - row_w) / 2.0;
            let min = pos2(
                left + col as f32 * (size.x + gap),
                top + row as f32 * (size.y + gap),
            );
            Rect::from_min_size(min, size)
        })
        .collect()
}

/// The largest 16:9 tile that fits `cols` x `rows` in `area`.
fn tile_size(cols: usize, rows: usize, area: Vec2, gap: f32) -> Vec2 {
    let w = (area.x - (cols - 1) as f32 * gap) / cols as f32;
    let h = (area.y - (rows - 1) as f32 * gap) / rows as f32;
    let w = w.min(h * ASPECT).max(0.0);
    vec2(w, w / ASPECT)
}

/// Speaker view: one large tile above a strip of small ones. Returns (stage, strip tiles).
/// The strip holds as many as fit; the rest are not shown.
pub fn speaker(others: usize, area: Rect, gap: f32) -> (Rect, Vec<Rect>) {
    if others == 0 {
        let stage = gallery(1, area, gap);
        return (stage.first().copied().unwrap_or(area), Vec::new());
    }
    let strip_h = (area.height() * 0.18).clamp(72.0, 140.0);
    let stage_area =
        Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - strip_h - gap));
    let stage = gallery(1, stage_area, gap)
        .first()
        .copied()
        .unwrap_or(stage_area);
    let tile = vec2(strip_h * ASPECT, strip_h);
    let fit = (((area.width() + gap) / (tile.x + gap)) as usize).max(1);
    let shown = others.min(fit);
    let row_w = shown as f32 * tile.x + (shown - 1) as f32 * gap;
    let left = area.left() + (area.width() - row_w) / 2.0;
    let strip = (0..shown)
        .map(|i| {
            Rect::from_min_size(
                pos2(left + i as f32 * (tile.x + gap), area.bottom() - strip_h),
                tile,
            )
        })
        .collect();
    (stage, strip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    fn check(tiles: &[Rect], area: Rect) {
        for (i, a) in tiles.iter().enumerate() {
            assert!(
                area.expand(0.01).contains_rect(*a),
                "{a:?} outside {area:?}"
            );
            assert!((a.width() / a.height() - ASPECT).abs() < 1e-3);
            for b in &tiles[i + 1..] {
                assert!(!a.shrink(0.01).intersects(*b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_wide_window_puts_two_side_by_side() {
        let a = area(1600.0, 600.0);
        let tiles = gallery(2, a, 8.0);
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].top(), tiles[1].top());
        check(&tiles, a);
    }

    #[test]
    fn five_people_make_three_and_two_with_the_short_row_centred() {
        let a = area(1280.0, 720.0);
        let tiles = gallery(5, a, 8.0);
        check(&tiles, a);
        assert_eq!(tiles[0].top(), tiles[2].top());
        assert!(tiles[3].top() > tiles[0].top());
        let row_centre = (tiles[3].left() + tiles[4].right()) / 2.0;
        assert!((row_centre - 640.0).abs() < 0.5);
    }

    #[test]
    fn many_tiles_still_fit() {
        for count in [1, 3, 9, 16, 25] {
            let a = area(900.0, 900.0);
            check(&gallery(count, a, 8.0), a);
        }
        assert!(gallery(3, area(0.0, 100.0), 8.0).is_empty());
    }

    #[test]
    fn speaker_view_has_a_stage_and_a_strip() {
        let a = area(1280.0, 720.0);
        let (stage, strip) = speaker(4, a, 8.0);
        assert_eq!(strip.len(), 4);
        assert!(strip.iter().all(|t| t.top() > stage.bottom()));
        let mut all = strip.clone();
        all.push(stage);
        check(&all, a);
        let (_, crowded) = speaker(40, a, 8.0);
        assert!(crowded.len() < 40);
    }
}
