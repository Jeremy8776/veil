//! The phone's Focus stage, in the FaceTime manner: the speaker fills the stage edge to
//! edge, your own picture floats small in the bottom-right corner, and everyone else sits
//! in a row of small tiles along the bottom.
//!
//! Invariants: pictures here fill their tile and are cropped to it (unlike the desktop card
//! rule), the speaker is drawn first so the small tiles float above it, and tiles keep
//! their identity keys so a new speaker glides onto the stage.
use crate::app::VeilApp;
use crate::screens::{bento, call::CallView};
use crate::theme;
use crate::widgets::{self, Tile};
use eframe::egui::{self, Color32, CornerRadius, Rect, Stroke, StrokeKind, pos2, vec2};

const INSET: f32 = 16.0;
const GAP: f32 = 8.0;
/// The longest side of a small tile.
const SMALL: f32 = 88.0;

/// Where each part of the stage goes.
#[derive(Debug, PartialEq)]
pub struct Stage {
    pub hero: Rect,
    pub you: Option<Rect>,
    pub others: Vec<Rect>,
}

/// Lays out the stage. `you` is your picture's aspect, if you are shown.
pub fn arrange(area: Rect, you: Option<f32>, others: usize) -> Stage {
    let you = you.map(|aspect| {
        let aspect = if aspect.is_finite() {
            aspect.clamp(0.56, 1.78)
        } else {
            0.75
        };
        let width = (area.width() * 0.28).clamp(72.0, 132.0);
        let height = (width / aspect).min(area.height() * 0.3);
        let width = height * aspect;
        Rect::from_min_max(
            area.max - vec2(INSET + width, INSET + height),
            area.max - vec2(INSET, INSET),
        )
    });
    let right = you.map_or(area.right() - INSET, |r| r.left() - GAP);
    let room = (right - area.left() - INSET).max(0.0);
    let n = others as f32;
    let size = SMALL.min(((room - GAP * (n - 1.0)) / n.max(1.0)).max(0.0));
    let bottom = area.bottom() - INSET;
    let others = (0..others)
        .map(|i| {
            let left = area.left() + INSET + i as f32 * (size + GAP);
            Rect::from_min_max(pos2(left, bottom - size), pos2(left + size, bottom))
        })
        .collect();
    Stage {
        hero: area,
        you,
        others,
    }
}

/// Texture coordinates that fill `rect` from a picture of `size`, cropping the overflow
/// evenly from both sides.
pub fn cover_uv(size: [usize; 2], rect: Rect) -> Rect {
    let picture = size[0].max(1) as f32 / size[1].max(1) as f32;
    let shape = rect.width().max(1.0) / rect.height().max(1.0);
    let (u, v) = if picture > shape {
        (shape / picture, 1.0)
    } else {
        (1.0, picture / shape)
    };
    Rect::from_center_size(pos2(0.5, 0.5), vec2(u, v))
}

pub fn show(view: &mut CallView, app: &VeilApp, ui: &mut egui::Ui, area: Rect) {
    view.update_focus();
    let people = &view.call.participants;
    let hero_id = view
        .focus
        .map(|f| f.id)
        .filter(|id| people.iter().any(|p| p.id == *id && !p.is_you))
        .or_else(|| people.iter().find(|p| !p.is_you).map(|p| p.id));
    let hero = people.iter().find(|p| Some(p.id) == hero_id);
    let you = people.iter().find(|p| p.is_you);
    let others: Vec<_> = people
        .iter()
        .filter(|p| !p.is_you && Some(p.id) != hero_id)
        .collect();
    let you_aspect =
        you.map(|p| bento::video(app, p).map_or(0.75, |(_, s)| s[0] as f32 / s[1].max(1) as f32));
    let stage = arrange(area, you_aspect, others.len());
    // Speaker first, so the small tiles are drawn over it; you last, on top.
    let mut placed: Vec<(&crate::model::Participant, Rect)> = Vec::new();
    if let Some(hero) = hero {
        placed.push((hero, stage.hero));
    }
    placed.extend(others.iter().copied().zip(stage.others.iter().copied()));
    if let (Some(you), Some(rect)) = (you, stage.you) {
        placed.push((you, rect));
    }
    let scale = ui.ctx().pixels_per_point();
    view.watch_tiles(
        placed
            .iter()
            .filter(|(p, _)| !p.is_you)
            .map(|(p, r)| (p.id, r.width() * scale))
            .collect(),
    );
    let targets: Vec<_> = placed.iter().map(|(p, r)| (u64::from(p.id), *r)).collect();
    let duration = if ui.style().animation_time <= 0.0 {
        0.0
    } else {
        0.24
    };
    let frame = view
        .tile_motion
        .frame(&targets, ui.input(|input| input.time), duration);
    if frame.moving {
        ui.ctx().request_repaint();
    }
    let painter = ui.painter_at(area);
    for placement in frame.cells {
        let Some((person, _)) = placed
            .iter()
            .find(|(p, _)| u64::from(p.id) == placement.key)
        else {
            continue;
        };
        let small = Some(person.id) != hero_id;
        let tile = Tile {
            name: &person.name,
            video: bento::video(app, person),
            muted: person.muted,
            speaking: person.speaking && small,
            hand_raised: person.hand_raised,
            is_you: person.is_you,
        };
        let mut layer = painter.clone();
        layer.set_opacity(placement.opacity);
        draw(&layer, placement.rect, &tile, small);
    }
}

fn draw(painter: &egui::Painter, rect: Rect, tile: &Tile<'_>, small: bool) {
    let radius = CornerRadius::same(if small { 12 } else { 16 });
    match tile.video {
        Some((texture, size)) => {
            painter.add(
                egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
                    .with_texture(texture, cover_uv(size, rect)),
            );
        }
        None => {
            painter.rect_filled(rect, radius, theme::TILE);
            let r = (rect.height() * 0.2).clamp(14.0, 64.0);
            widgets::avatar(painter, rect.center(), r, tile.name);
        }
    }
    if small {
        // A thin edge keeps a small tile apart from the picture behind it.
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, Color32::from_white_alpha(40)),
            StrokeKind::Inside,
        );
    }
    if tile.speaking {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(2.5, theme::SPEAKING),
            StrokeKind::Inside,
        );
    }
    // FaceTime labels nobody's own picture, and keeps small tiles clean.
    if !small {
        widgets::name_tag(painter, rect, tile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speaker_fills_the_stage_and_the_rest_float_inside_it() {
        let area = Rect::from_min_size(pos2(0.0, 80.0), vec2(390.0, 640.0));
        let stage = arrange(area, Some(9.0 / 16.0), 3);
        assert_eq!(stage.hero, area);
        let you = stage.you.expect("you are shown");
        assert!(
            (you.right() - (area.right() - INSET)).abs() < 0.01,
            "bottom-right corner"
        );
        assert!((you.bottom() - (area.bottom() - INSET)).abs() < 0.01);
        assert!(you.area() < area.area() * 0.1, "your picture stays small");
        assert_eq!(stage.others.len(), 3);
        for (i, a) in stage.others.iter().enumerate() {
            assert!(area.contains_rect(*a));
            assert!(a.intersect(you).area() < 1.0, "never under your picture");
            for b in &stage.others[i + 1..] {
                assert!(a.intersect(*b).area() < 1.0);
            }
        }
        // A big group shrinks its row instead of running off the stage.
        let crowd = arrange(area, Some(0.75), 12);
        assert!(
            crowd
                .others
                .iter()
                .all(|r| r.right() <= crowd.you.unwrap().left())
        );
    }

    #[test]
    fn cover_crops_evenly_and_never_stretches() {
        let square = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let uv = cover_uv([1600, 900], square);
        assert!((uv.width() - 900.0 / 1600.0).abs() < 0.001 && uv.height() == 1.0);
        assert!((uv.center().x - 0.5).abs() < 0.001);
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(90.0, 160.0));
        let uv = cover_uv([900, 1600], tall);
        assert!((uv.width() - 1.0).abs() < 0.01 && (uv.height() - 1.0).abs() < 0.01);
    }
}
