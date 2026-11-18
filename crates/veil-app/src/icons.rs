//! Icons from the system icon font (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on
//! Windows 10), or drawn as vector strokes where there is none (Linux, macOS).
//!
//! Purpose: one enum for every icon the app shows, so a codepoint is written once. The
//! codepoints were checked against both fonts' character maps and by rendering them.
//!
//! Invariants:
//! - Nothing here is bundled; the font belongs to the operating system.
//! - Without an icon font, [`Icon::paint`] draws the icon from strokes, so it needs no font.
//! - [`Icon::Mirror`] is always drawn, because the icon fonts have no clear mirror glyph.
//! - An "off" state (camera off) is the "on" icon with a slash painted across, not a
//!   separate glyph, so it looks the same in both fonts.

use std::sync::atomic::{AtomicBool, Ordering};

use std::f32::consts::{FRAC_PI_4, PI};

use eframe::egui::{
    Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2,
};

use crate::theme::ICON_FAMILY;

static HAVE_FONT: AtomicBool = AtomicBool::new(false);

/// Records whether the icon font loaded. Called once at start-up.
pub fn set_available(available: bool) {
    HAVE_FONT.store(available, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Mic,
    MicOff,
    Video,
    Share,
    People,
    Chat,
    Leave,
    Lock,
    Shield,
    Grid,
    Speaker,
    Copy,
    Close,
    Signal,
    Send,
    Link,
    Volume,
    Info,
    /// A palette ("Color" glyph), for video effects.
    Effects,
    /// A raised hand, from the symbol font (the icon fonts have none).
    Hand,
    /// A shape and its reflection either side of a dashed line, for mirroring your video.
    Mirror,
}

impl Icon {
    fn codepoint(self) -> Option<char> {
        let glyph = match self {
            Self::Mic => '\u{E720}',
            Self::MicOff => '\u{EC54}',
            Self::Video => '\u{E714}',
            Self::Share => '\u{E7F4}',
            Self::People => '\u{E716}',
            Self::Chat => '\u{E8BD}',
            Self::Leave => '\u{E778}',
            Self::Lock => '\u{E72E}',
            Self::Shield => '\u{EA18}',
            Self::Grid => '\u{E8A9}',
            Self::Speaker => '\u{E740}',
            Self::Copy => '\u{E8C8}',
            Self::Close => '\u{E711}',
            Self::Signal => '\u{E701}',
            Self::Send => '\u{E724}',
            Self::Link => '\u{E71B}',
            Self::Volume => '\u{E767}',
            Self::Info => '\u{E946}',
            Self::Effects => '\u{E790}',
            Self::Hand => '\u{270B}',
            Self::Mirror => return None,
        };
        Some(glyph)
    }

    /// The glyph, when the icon font loaded.
    pub fn glyph(self) -> Option<char> {
        HAVE_FONT.load(Ordering::Relaxed).then(|| self.codepoint()).flatten()
    }

    /// Paints the icon centred at `center`: the font glyph, or vector strokes without it.
    pub fn paint(self, painter: &Painter, center: Pos2, size: f32, colour: Color32) -> Rect {
        if let Some(glyph) = self.glyph() {
            let font = FontId::new(size, FontFamily::Name(ICON_FAMILY.into()));
            return painter.text(center, Align2::CENTER_CENTER, glyph, font, colour);
        }
        painter.extend(self.shapes(center, size, colour));
        Rect::from_center_size(center, vec2(size, size))
    }

    /// The icon as strokes in a `size` square around `center`, for systems without an icon
    /// font (Linux, macOS). Coordinates are in icon units: -0.5..0.5 each way, y down.
    fn shapes(self, center: Pos2, size: f32, colour: Color32) -> Vec<Shape> {
        let w = (size * 0.09).max(1.0);
        let stroke = Stroke::new(w, colour);
        let p = |x: f32, y: f32| center + vec2(x, y) * size;
        let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_two_pos(p(x0, y0), p(x1, y1));
        let poly =
            |pts: &[(f32, f32)]| Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke);
        let closed = |pts: &[(f32, f32)]| {
            Shape::closed_line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke)
        };
        let boxed = |rect: Rect, radius: f32| {
            Shape::rect_stroke(rect, radius * size, stroke, StrokeKind::Middle)
        };
        // An arc of radius `rad` around (x, y), from angle `a0` to `a1` (radians, y down).
        let arc = |x: f32, y: f32, rad: f32, a0: f32, a1: f32| {
            let steps = 16;
            let pts = (0..=steps)
                .map(|i| {
                    let a = a0 + (a1 - a0) * i as f32 / steps as f32;
                    p(x + rad * a.cos(), y + rad * a.sin())
                })
                .collect();
            Shape::line(pts, stroke)
        };
        let slash = Shape::line_segment([p(-0.42, -0.42), p(0.42, 0.42)], stroke);
        let mic = || {
            vec![
                boxed(r(-0.14, -0.45, 0.14, 0.1), 0.14),
                arc(0.0, -0.05, 0.28, 0.0, PI),
                poly(&[(0.0, 0.23), (0.0, 0.42)]),
                poly(&[(-0.15, 0.42), (0.15, 0.42)]),
            ]
        };
        match self {
            Self::Mic => mic(),
            Self::MicOff => {
                let mut shapes = mic();
                shapes.push(slash);
                shapes
            }
            Self::Video => vec![
                boxed(r(-0.45, -0.25, 0.15, 0.25), 0.06),
                closed(&[(0.15, -0.06), (0.45, -0.22), (0.45, 0.22), (0.15, 0.06)]),
            ],
            Self::Share => vec![
                boxed(r(-0.45, -0.36, 0.45, 0.24), 0.05),
                poly(&[(-0.2, 0.42), (0.2, 0.42)]),
                poly(&[(0.0, 0.12), (0.0, -0.2)]),
                poly(&[(-0.12, -0.08), (0.0, -0.2), (0.12, -0.08)]),
            ],
            Self::People => vec![
                Shape::circle_stroke(p(-0.12, -0.18), 0.13 * size, stroke),
                arc(-0.12, 0.42, 0.28, PI, 2.0 * PI),
                Shape::circle_stroke(p(0.24, -0.12), 0.1 * size, stroke),
                arc(0.24, 0.42, 0.2, 1.3 * PI, 2.0 * PI),
            ],
            Self::Chat => vec![
                boxed(r(-0.45, -0.38, 0.45, 0.2), 0.08),
                poly(&[(-0.22, 0.2), (-0.3, 0.42), (0.02, 0.2)]),
            ],
            Self::Leave => vec![
                poly(&[(0.05, -0.42), (-0.4, -0.42), (-0.4, 0.42), (0.05, 0.42)]),
                poly(&[(-0.12, 0.0), (0.45, 0.0)]),
                poly(&[(0.28, -0.16), (0.45, 0.0), (0.28, 0.16)]),
            ],
            Self::Lock => vec![
                boxed(r(-0.32, -0.05, 0.32, 0.45), 0.06),
                poly(&[(-0.2, -0.05), (-0.2, -0.17)]),
                arc(0.0, -0.17, 0.2, PI, 2.0 * PI),
                poly(&[(0.2, -0.17), (0.2, -0.05)]),
            ],
            Self::Shield => vec![closed(&[
                (0.0, -0.45),
                (0.38, -0.3),
                (0.38, 0.0),
                (0.3, 0.22),
                (0.0, 0.45),
                (-0.3, 0.22),
                (-0.38, 0.0),
                (-0.38, -0.3),
            ])],
            Self::Grid => vec![
                boxed(r(-0.42, -0.42, -0.06, -0.06), 0.04),
                boxed(r(0.06, -0.42, 0.42, -0.06), 0.04),
                boxed(r(-0.42, 0.06, -0.06, 0.42), 0.04),
                boxed(r(0.06, 0.06, 0.42, 0.42), 0.04),
            ],
            Self::Speaker => vec![
                boxed(r(-0.45, -0.4, 0.45, 0.14), 0.04),
                boxed(r(-0.45, 0.26, -0.17, 0.42), 0.02),
                boxed(r(-0.14, 0.26, 0.14, 0.42), 0.02),
                boxed(r(0.17, 0.26, 0.45, 0.42), 0.02),
            ],
            Self::Copy => vec![
                poly(&[
                    (-0.18, 0.18),
                    (-0.42, 0.18),
                    (-0.42, -0.42),
                    (0.18, -0.42),
                    (0.18, -0.18),
                ]),
                boxed(r(-0.18, -0.18, 0.42, 0.42), 0.04),
            ],
            Self::Close => vec![
                poly(&[(-0.32, -0.32), (0.32, 0.32)]),
                poly(&[(0.32, -0.32), (-0.32, 0.32)]),
            ],
            Self::Signal => [0.25, 0.45, 0.65, 0.85]
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let x = -0.42 + 0.24 * i as f32;
                    Shape::rect_filled(r(x, 0.42 - h, x + 0.14, 0.42), 0.0, colour)
                })
                .collect(),
            Self::Send => vec![
                closed(&[(-0.42, -0.4), (0.45, 0.0), (-0.42, 0.4), (-0.26, 0.0)]),
                poly(&[(-0.26, 0.0), (0.1, 0.0)]),
            ],
            Self::Link => vec![
                boxed(r(-0.48, -0.17, 0.04, 0.17), 0.17),
                boxed(r(-0.04, -0.17, 0.48, 0.17), 0.17),
            ],
            Self::Volume => vec![
                closed(&[
                    (-0.45, -0.12),
                    (-0.25, -0.12),
                    (0.0, -0.35),
                    (0.0, 0.35),
                    (-0.25, 0.12),
                    (-0.45, 0.12),
                ]),
                arc(0.0, 0.0, 0.2, -FRAC_PI_4, FRAC_PI_4),
                arc(0.0, 0.0, 0.38, -FRAC_PI_4, FRAC_PI_4),
            ],
            Self::Info => vec![
                Shape::circle_stroke(center, 0.42 * size, stroke),
                Shape::circle_filled(p(0.0, -0.18), w * 0.8, colour),
                poly(&[(0.0, -0.03), (0.0, 0.24)]),
            ],
            Self::Effects => vec![closed(&[
                (0.0, -0.45),
                (0.1, -0.1),
                (0.45, 0.0),
                (0.1, 0.1),
                (0.0, 0.45),
                (-0.1, 0.1),
                (-0.45, 0.0),
                (-0.1, -0.1),
            ])],
            Self::Hand => vec![
                boxed(r(-0.25, -0.05, 0.25, 0.45), 0.15),
                poly(&[(-0.18, -0.05), (-0.18, -0.32)]),
                poly(&[(-0.06, -0.05), (-0.06, -0.42)]),
                poly(&[(0.06, -0.05), (0.06, -0.42)]),
                poly(&[(0.18, -0.05), (0.18, -0.32)]),
                poly(&[(-0.25, 0.12), (-0.42, -0.08)]),
            ],
            Self::Mirror => vec![
                poly(&[(0.0, -0.45), (0.0, -0.27)]),
                poly(&[(0.0, -0.09), (0.0, 0.09)]),
                poly(&[(0.0, 0.27), (0.0, 0.45)]),
                Shape::convex_polygon(
                    vec![p(-0.13, -0.3), p(-0.13, 0.3), p(-0.45, 0.3)],
                    colour,
                    stroke,
                ),
                closed(&[(0.13, -0.3), (0.13, 0.3), (0.45, 0.3)]),
            ],
        }
    }

    /// Paints the icon with a slash across it, for a switched-off state. `behind` is the
    /// colour under the icon, used to cut a gap either side of the slash.
    pub fn paint_off(
        self,
        painter: &Painter,
        center: Pos2,
        size: f32,
        colour: Color32,
        behind: Color32,
    ) {
        self.paint(painter, center, size, colour);
        let half = size * 0.5;
        let a = center + vec2(-half, -half);
        let b = center + vec2(half, half);
        painter.line_segment([a, b], Stroke::new(size * 0.2, behind));
        painter.line_segment([a, b], Stroke::new(size * 0.08, colour));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 20] = [
        Icon::Mic,
        Icon::MicOff,
        Icon::Video,
        Icon::Share,
        Icon::People,
        Icon::Chat,
        Icon::Leave,
        Icon::Lock,
        Icon::Shield,
        Icon::Grid,
        Icon::Speaker,
        Icon::Copy,
        Icon::Close,
        Icon::Signal,
        Icon::Send,
        Icon::Link,
        Icon::Volume,
        Icon::Info,
        Icon::Hand,
        Icon::Mirror,
    ];

    #[test]
    fn the_mirror_icon_never_uses_the_font() {
        set_available(true);
        assert_eq!(Icon::Mirror.glyph(), None);
    }

    #[test]
    fn drawn_icons_stay_inside_their_square() {
        let size = 20.0;
        let center = Pos2::new(100.0, 100.0);
        // Half a stroke may spill past the edge.
        let square = Rect::from_center_size(center, vec2(size, size)).expand(size * 0.05);
        for icon in ALL {
            let shapes = icon.shapes(center, size, Color32::WHITE);
            assert!(!shapes.is_empty(), "{icon:?} draws nothing");
            for shape in shapes {
                let bounds = shape.visual_bounding_rect();
                assert!(
                    square.contains_rect(bounds),
                    "{icon:?} spills out: {bounds:?}"
                );
            }
        }
    }
}
