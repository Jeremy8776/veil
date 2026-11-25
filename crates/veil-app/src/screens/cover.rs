//! Tiles that fill their space and crop the picture to it, framed on the person's head
//! (`crate::framing`). Used by the phone's Focus stage and the desktop Focus strip.
//!
//! Invariants: the crop never stretches the picture and never leaves the picture's edges;
//! with no head found it is centred; the framing point glides rather than jumps.
use crate::app::VeilApp;
use crate::framing;
use crate::model::Participant;
use crate::theme;
use crate::widgets::{self, Tile};
use eframe::egui::{self, Color32, CornerRadius, Rect, Stroke, StrokeKind, pos2, vec2};

/// Where the head sits in a crop that is shorter than the picture: a little above middle.
const HEAD_HEIGHT: f32 = 0.42;

/// Texture coordinates that fill `rect` from a picture of `size`, keeping `centre` (a
/// fraction of the picture) in view, or the middle when there is none.
pub fn cover_uv(size: [usize; 2], rect: Rect, centre: Option<[f32; 2]>) -> Rect {
    let picture = size[0].max(1) as f32 / size[1].max(1) as f32;
    let shape = rect.width().max(1.0) / rect.height().max(1.0);
    let (u, v) = if picture > shape {
        (shape / picture, 1.0)
    } else {
        (1.0, picture / shape)
    };
    let [cx, cy] = centre.unwrap_or([0.5, 0.5]);
    let left = (cx - u / 2.0).clamp(0.0, 1.0 - u);
    let top = if centre.is_some() {
        (cy - v * HEAD_HEIGHT).clamp(0.0, 1.0 - v)
    } else {
        (1.0 - v) / 2.0
    };
    Rect::from_min_size(pos2(left, top), vec2(u, v))
}

/// Where `person`'s head is, smoothed so the crop glides.
fn centre(ui: &egui::Ui, app: &VeilApp, person: &Participant) -> Option<[f32; 2]> {
    let key = if person.is_you {
        framing::YOU
    } else {
        person.id
    };
    let [x, y] = app.framing.centre(key)?;
    let ctx = ui.ctx();
    let glide = |axis: &str, value| {
        ctx.animate_value_with_time(egui::Id::unique(("framing", key, axis)), value, 0.6)
    };
    Some([glide("x", x), glide("y", y)])
}

/// Paints `person` filling `rect`. `small` tiles get a thin edge and no name tag, the
/// FaceTime way; `name` puts the name tag on a large one.
pub fn tile(
    ui: &egui::Ui,
    painter: &egui::Painter,
    app: &VeilApp,
    rect: Rect,
    person: &Participant,
    small: bool,
) {
    app.framing.want();
    let tile = Tile {
        name: &person.name,
        video: crate::screens::bento::video(app, person),
        muted: person.muted,
        speaking: person.speaking && small,
        hand_raised: person.hand_raised,
        is_you: person.is_you,
    };
    let radius = CornerRadius::same(if small { 12 } else { 16 });
    match tile.video {
        Some((texture, size)) => {
            let uv = cover_uv(size, rect, centre(ui, app, person));
            painter.add(
                egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
                    .with_texture(texture, uv),
            );
        }
        None => {
            painter.rect_filled(rect, radius, theme::TILE);
            let r = (rect.height() * 0.2).clamp(14.0, 64.0);
            widgets::avatar(painter, rect.center(), r, tile.name);
        }
    }
    if small {
        // A thin edge keeps a small tile apart from what is behind it.
        let edge = Stroke::new(1.0, Color32::from_white_alpha(40));
        painter.rect_stroke(rect, radius, edge, StrokeKind::Inside);
    }
    if tile.speaking {
        let ring = Stroke::new(2.5, theme::SPEAKING);
        painter.rect_stroke(rect, radius, ring, StrokeKind::Inside);
    }
    if !small || (!person.is_you && rect.height() >= 120.0) {
        widgets::name_tag(painter, rect, &tile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    #[test]
    fn cover_crops_evenly_and_never_stretches() {
        let uv = cover_uv([1600, 900], rect(100.0, 100.0), None);
        assert!((uv.width() - 900.0 / 1600.0).abs() < 0.001 && uv.height() == 1.0);
        assert!((uv.center().x - 0.5).abs() < 0.001);
        let uv = cover_uv([900, 1600], rect(90.0, 160.0), None);
        assert!((uv.width() - 1.0).abs() < 0.01 && (uv.height() - 1.0).abs() < 0.01);
    }

    #[test]
    fn the_crop_follows_the_head_but_stays_inside_the_picture() {
        // A landscape picture in a portrait tile, the head well left of centre.
        let uv = cover_uv([1280, 720], rect(90.0, 160.0), Some([0.3, 0.4]));
        assert!((uv.center().x - 0.3).abs() < 0.001, "{uv:?}");
        // At the edge the crop stops at the picture's edge rather than showing past it.
        let uv = cover_uv([1280, 720], rect(90.0, 160.0), Some([0.02, 0.4]));
        assert_eq!(uv.left(), 0.0);
        let uv = cover_uv([1280, 720], rect(90.0, 160.0), Some([0.99, 0.4]));
        assert!((uv.right() - 1.0).abs() < 0.001);
        // A portrait picture in a wide tile keeps the head a little above middle.
        let uv = cover_uv([720, 1280], rect(160.0, 90.0), Some([0.5, 0.3]));
        assert!(uv.top() > 0.0 && uv.top() < 0.3 && uv.bottom() <= 1.0);
    }
}
