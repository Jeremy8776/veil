//! Aspect-aware bento geometry shared by desktop and mobile call stages.
//! A featured participant or share gets the primary cell; every member stays visible.
//! Pictures fit inside their cells without cropping. Motion is keyed by session identity.
mod motion;
use eframe::egui::{Rect, pos2, vec2};
pub use motion::Motion;
pub const ASPECT: f32 = 16.0 / 9.0;

fn aspect(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(0.2, 5.0)
    } else {
        ASPECT
    }
}

/// Fit the entire image. Its texture coordinates always remain 0..1.
pub fn contain(size: [usize; 2], area: Rect) -> Rect {
    let ratio = size[0].max(1) as f32 / size[1].max(1) as f32;
    let width = area.width().min(area.height() * ratio).max(0.0);
    Rect::from_center_size(area.center(), vec2(width, width / ratio))
}

fn visible_area(ratio: f32, area: Rect) -> f32 {
    let ratio = aspect(ratio);
    let width = area
        .width()
        .min((area.height() - 36.0).max(0.0) * ratio)
        .max(0.0);
    width * width / ratio
}

/// Fill a region; the last row expands rather than leaving an empty cell.
fn grid(aspects: &[f32], area: Rect, gap: f32) -> Vec<Rect> {
    let gap = gap.min(area.width().min(area.height()) / (aspects.len() + 1) as f32);
    let mut best = Vec::new();
    let mut best_score = -1.0;
    for columns in 1..=aspects.len() {
        let rows = aspects.len().div_ceil(columns);
        let height = (area.height() - gap * (rows - 1) as f32) / rows as f32;
        let mut cells = Vec::with_capacity(aspects.len());
        for index in 0..aspects.len() {
            let row = index / columns;
            let in_row = columns.min(aspects.len() - row * columns);
            let width = (area.width() - gap * (in_row - 1) as f32) / in_row as f32;
            cells.push(Rect::from_min_size(
                area.min
                    + vec2(
                        (index % columns) as f32 * (width + gap),
                        row as f32 * (height + gap),
                    ),
                vec2(width, height),
            ));
        }
        let score: f32 = cells
            .iter()
            .zip(aspects)
            .map(|(cell, ratio)| visible_area(*ratio, *cell))
            .sum();
        if score > best_score {
            best_score = score;
            best = cells;
        }
    }
    // Alternating wide and paired rows give medium groups a mosaic rather than a rail
    // of tiny thumbnails. Use it when it preserves more visible image area.
    if aspects.len() >= 3 {
        let mut groups = Vec::new();
        let mut remaining = aspects.len();
        while remaining > 0 {
            let take = if groups.len().is_multiple_of(2) { 1 } else { 2 }.min(remaining);
            groups.push(take);
            remaining -= take;
        }
        let height = (area.height() - gap * (groups.len() - 1) as f32) / groups.len() as f32;
        let mut cells = Vec::new();
        for (row, count) in groups.into_iter().enumerate() {
            let width = (area.width() - gap * (count - 1) as f32) / count as f32;
            for column in 0..count {
                cells.push(Rect::from_min_size(
                    area.min + vec2(column as f32 * (width + gap), row as f32 * (height + gap)),
                    vec2(width, height),
                ));
            }
        }
        let score: f32 = cells
            .iter()
            .zip(aspects)
            .map(|(cell, ratio)| visible_area(*ratio, *cell))
            .sum();
        if score > best_score {
            best = cells;
        }
    }
    best
}

/// Responsive mosaic in input order, trying tall/wide cells against source aspect ratios.
pub fn bento(aspects: &[f32], featured: usize, area: Rect, gap: f32) -> Vec<Rect> {
    let count = aspects.len();
    if count == 0 || !area.is_finite() || area.width() <= 0.0 || area.height() <= 0.0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![area];
    }
    let gap = gap
        .max(0.0)
        .min(area.width().min(area.height()) / (count + 1) as f32);
    let featured = featured.min(count - 1);
    let remaining: Vec<f32> = aspects
        .iter()
        .enumerate()
        .filter_map(|(i, a)| (i != featured).then_some(*a))
        .collect();
    let mut best = Vec::new();
    let mut best_score = -1.0;
    for horizontal in [true, false] {
        for weight in [0.55, 0.62, 0.68] {
            let (hero, rest) = if horizontal {
                let edge = area.left() + (area.width() - gap) * weight;
                (
                    Rect::from_min_max(area.min, pos2(edge, area.bottom())),
                    Rect::from_min_max(pos2(edge + gap, area.top()), area.max),
                )
            } else {
                let edge = area.top() + (area.height() - gap) * weight;
                (
                    Rect::from_min_max(area.min, pos2(area.right(), edge)),
                    Rect::from_min_max(pos2(area.left(), edge + gap), area.max),
                )
            };
            let others = grid(&remaining, rest, gap);
            let score = visible_area(aspects[featured], hero) * 1.15
                + others
                    .iter()
                    .zip(&remaining)
                    .map(|(r, a)| visible_area(*a, *r))
                    .sum::<f32>();
            if score > best_score {
                best_score = score;
                let mut others = others.into_iter();
                best = (0..count)
                    .filter_map(|i| {
                        if i == featured {
                            Some(hero)
                        } else {
                            others.next()
                        }
                    })
                    .collect();
            }
        }
    }
    best
}

/// Focus view: the featured cell fills the stage and everyone else sits small in one strip,
/// along the bottom on a tall area and down the right on a wide one. Input order is kept.
pub fn focus(aspects: &[f32], featured: usize, area: Rect, gap: f32) -> Vec<Rect> {
    let count = aspects.len();
    if count == 0 || !area.is_finite() || area.width() <= 0.0 || area.height() <= 0.0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![area];
    }
    let featured = featured.min(count - 1);
    let gap = gap.max(0.0).min(area.width().min(area.height()) / 4.0);
    let rest: Vec<f32> = (0..count)
        .filter(|i| *i != featured)
        .map(|i| aspects[i])
        .collect();
    let tall = area.height() >= area.width();
    let (hero, strip) = if tall {
        let edge = area.bottom() - (area.height() * 0.2).min(150.0);
        (
            Rect::from_min_max(area.min, pos2(area.right(), edge - gap)),
            Rect::from_min_max(pos2(area.left(), edge), area.max),
        )
    } else {
        let edge = area.right() - (area.width() * 0.2).min(240.0);
        (
            Rect::from_min_max(area.min, pos2(edge - gap, area.bottom())),
            Rect::from_min_max(pos2(edge, area.top()), area.max),
        )
    };
    // One row (or column): cells share the strip evenly, so a big group shrinks rather than
    // wrapping over the stage.
    let n = rest.len() as f32;
    let mut others = (0..rest.len()).map(|i| {
        let i = i as f32;
        if tall {
            let w = (strip.width() - gap * (n - 1.0)) / n;
            Rect::from_min_size(strip.min + vec2(i * (w + gap), 0.0), vec2(w, strip.height()))
        } else {
            let h = (strip.height() - gap * (n - 1.0)) / n;
            Rect::from_min_size(strip.min + vec2(0.0, i * (h + gap)), vec2(strip.width(), h))
        }
    });
    (0..count)
        .filter_map(|i| if i == featured { Some(hero) } else { others.next() })
        .collect()
}

/// Justified rows in input order: every cell has exactly its source's aspect ratio, so a
/// card that hugs its picture fills its cell and no space is wasted inside cells. Tries
/// every way to break the list into rows (up to [`JUSTIFY_MAX`] people) and keeps the one
/// that shows the most picture, then centres the block in `area`. `None` past the limit.
pub fn justified(aspects: &[f32], area: Rect, gap: f32) -> Option<Vec<Rect>> {
    let count = aspects.len();
    if count == 0 || count > JUSTIFY_MAX || !area.is_finite() || area.width() <= 0.0 || area.height() <= 0.0 {
        return None;
    }
    let aspects: Vec<f32> = aspects.iter().map(|a| a.clamp(0.25, 4.0)).collect();
    let mut best: Option<(f32, Vec<usize>, f32)> = None;
    // Bit i set = a new row starts after item i.
    for mask in 0u32..(1 << (count - 1)) {
        let rows = row_lengths(mask, count);
        let (scale, area_shown) = fit(&aspects, &rows, area, gap);
        if best.as_ref().is_none_or(|(shown, _, _)| area_shown > *shown) {
            best = Some((area_shown, rows, scale));
        }
    }
    let (_, rows, scale) = best?;
    let heights: Vec<f32> = row_heights(&aspects, &rows, area.width(), gap)
        .into_iter()
        .map(|h| h * scale)
        .collect();
    let block = heights.iter().sum::<f32>() + gap * (rows.len() - 1) as f32;
    let mut y = area.top() + ((area.height() - block) * 0.5).max(0.0);
    let mut cells = Vec::with_capacity(count);
    let mut start = 0;
    for (len, height) in rows.iter().zip(&heights) {
        let row = &aspects[start..start + len];
        let width = row.iter().map(|a| a * height).sum::<f32>() + gap * (len - 1) as f32;
        let mut x = area.left() + ((area.width() - width) * 0.5).max(0.0);
        for aspect in row {
            let cell = Rect::from_min_size(pos2(x, y), vec2(aspect * height, *height));
            x = cell.right() + gap;
            cells.push(cell);
        }
        y += height + gap;
        start += len;
    }
    Some(cells)
}

/// Above this many people, justified rows are not searched (2^(n-1) splits).
pub const JUSTIFY_MAX: usize = 9;

fn row_lengths(mask: u32, count: usize) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut len = 0;
    for i in 0..count {
        len += 1;
        if i + 1 == count || mask & (1 << i) != 0 {
            rows.push(len);
            len = 0;
        }
    }
    rows
}

/// Each row's height when it spans the full `width`.
fn row_heights(aspects: &[f32], rows: &[usize], width: f32, gap: f32) -> Vec<f32> {
    let mut start = 0;
    rows.iter()
        .map(|len| {
            let sum: f32 = aspects[start..start + len].iter().sum();
            start += len;
            ((width - gap * (len - 1) as f32).max(1.0)) / sum
        })
        .collect()
}

/// The scale that fits the rows into `area`, and the picture area shown at that scale.
fn fit(aspects: &[f32], rows: &[usize], area: Rect, gap: f32) -> (f32, f32) {
    let heights = row_heights(aspects, rows, area.width(), gap);
    let room = (area.height() - gap * (rows.len() - 1) as f32).max(1.0);
    let scale = (room / heights.iter().sum::<f32>()).min(1.0);
    let mut start = 0;
    let shown = rows
        .iter()
        .zip(&heights)
        .map(|(len, h)| {
            let h = h * scale;
            let row: f32 = aspects[start..start + len].iter().map(|a| a * h * h).sum();
            start += len;
            row
        })
        .sum();
    (scale, shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_puts_one_large_stage_above_a_strip() {
        for (size, tall) in [(vec2(390.0, 700.0), true), (vec2(1280.0, 720.0), false)] {
            let area = Rect::from_min_size(pos2(5.0, 5.0), size);
            let cells = focus(&[ASPECT; 4], 2, area, 12.0);
            assert_eq!(cells.len(), 4);
            let hero = cells[2];
            for (i, cell) in cells.iter().enumerate() {
                assert!(area.expand(0.5).contains_rect(*cell));
                if i != 2 {
                    assert!(cell.area() * 3.0 < hero.area(), "the stage is largest");
                    assert!(cell.intersect(hero).area() < 1.0, "the strip is apart");
                    if tall {
                        assert!(cell.top() > hero.bottom());
                    } else {
                        assert!(cell.left() > hero.right());
                    }
                }
            }
        }
        let one = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        assert_eq!(focus(&[1.0], 0, one, 12.0), vec![one]);
        assert!(focus(&[], 0, one, 12.0).is_empty());
    }

    #[test]
    fn justified_cells_keep_their_shape_and_stay_inside() {
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(390.0, 600.0));
        let aspects = [16.0 / 9.0, 9.0 / 16.0, 1.25];
        let cells = justified(&aspects, area, 12.0).expect("three people fit");
        assert_eq!(cells.len(), 3);
        for (cell, aspect) in cells.iter().zip(aspects) {
            assert!((cell.width() / cell.height() - aspect).abs() < 0.01);
            assert!(area.expand(0.5).contains_rect(*cell));
        }
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                assert!(a.intersect(*b).area() < 1.0, "cells overlap");
            }
        }
        assert!(justified(&[1.0; JUSTIFY_MAX + 1], area, 12.0).is_none());
    }
    #[test]
    fn mixed_portrait_and_landscape_members_fit_without_overlap_at_every_size() {
        for size in [
            vec2(360.0, 640.0),
            vec2(740.0, 320.0),
            vec2(1280.0, 720.0),
            vec2(1.0, 1.0),
        ] {
            let area = Rect::from_min_size(pos2(17.0, 31.0), size);
            for count in 1..=32 {
                let aspects: Vec<_> = (0..count)
                    .map(|i| if i % 2 == 0 { 9.0 / 16.0 } else { ASPECT })
                    .collect();
                let cells = bento(&aspects, count - 1, area, 12.0);
                assert_eq!(cells.len(), count);
                for (i, cell) in cells.iter().enumerate() {
                    assert!(
                        cell.is_positive() && area.expand(0.001).contains_rect(*cell),
                        "count={count}, area={area:?}, cell={cell:?}"
                    );
                    for other in &cells[i + 1..] {
                        assert!(!cell.shrink(0.0001).intersects(*other));
                    }
                }
            }
        }
    }
    #[test]
    fn a_portrait_source_keeps_its_entire_vertical_frame_in_a_wide_tile() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0));
        let image = contain([480, 640], area);
        assert_eq!(image.height(), area.height());
        assert!((image.width() / image.height() - 0.75).abs() < 0.001);
        assert_eq!(image.center(), area.center());
        let landscape = contain(
            [1920, 1080],
            Rect::from_min_size(area.min, vec2(360.0, 640.0)),
        );
        assert_eq!(landscape.width(), 360.0);
        assert!((landscape.height() - 202.5).abs() < 0.001);
    }
    #[test]
    fn hierarchy_is_distinct_and_empty_areas_have_no_cells() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0));
        let cells = bento(&[ASPECT; 6], 2, area, 12.0);
        assert!(cells[2].area() > cells[0].area());
        assert!(bento(&[], 0, area, 12.0).is_empty());
        assert!(bento(&[ASPECT], 0, Rect::NOTHING, 12.0).is_empty());
    }
}
