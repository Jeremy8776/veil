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

#[cfg(test)]
mod tests {
    use super::*;
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
