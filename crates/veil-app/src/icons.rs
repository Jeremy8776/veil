//! Icons from an icon font: the system's Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets
//! (Windows 10), or Veil's bundled Phosphor subset where there is no system icon font (macOS,
//! Linux).
//!
//! Purpose: one enum for every icon the app shows, so a codepoint is written once per font.
//! The Segoe codepoints were checked against both fonts' character maps and by rendering them.
//! The Phosphor codepoints come from the font's own stylesheet (`assets/phosphor/README.md`).
//!
//! Invariants:
//! - Only Phosphor is bundled: it is MIT licensed, which is on the allow list. It holds only
//!   the glyphs listed in [`Icon::phosphor`]; a test checks each is in the file.
//! - [`Icon::SpeakerView`] is always drawn from shapes, because no icon font has a clear glyph
//!   for it. [`Icon::Mirror`] is drawn on Windows, where Segoe has none.
//! - An "off" state (camera off) is the "on" icon with a slash painted across, not a
//!   separate glyph, so it looks the same in both fonts.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2,
};

use crate::theme::ICON_FAMILY;

/// The bundled Phosphor subset, for systems without an icon font.
pub const PHOSPHOR_TTF: &[u8] = include_bytes!("../assets/phosphor/PhosphorVeil.ttf");

/// Whether the system's Segoe icon font loaded; otherwise the bundled Phosphor is in use.
static SEGOE: AtomicBool = AtomicBool::new(false);

/// Records which icon font loaded. Called once at start-up.
pub fn set_segoe(segoe: bool) {
    SEGOE.store(segoe, Ordering::Relaxed);
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
    /// One large tile above a strip of small ones, the layout it switches to.
    SpeakerView,
    Copy,
    Close,
    Signal,
    Send,
    Link,
    Volume,
    Info,
    /// A palette ("Color" glyph), for video effects.
    Effects,
    /// A raised hand. On Windows it comes from the symbol font (Segoe has none).
    Hand,
    /// A smiling face, for the chat's emoji button.
    Emoji,
    /// A shape and its reflection either side of a dashed line, for mirroring your video.
    Mirror,
    /// A key, for the device key.
    Key,
    /// A folder, for opening the log folder.
    Folder,
    /// A play triangle, for the demo call.
    Play,
    /// A circular arrow, for checking for updates.
    Refresh,
}

impl Icon {
    /// The glyph in Segoe Fluent Icons and Segoe MDL2 Assets.
    fn segoe(self) -> Option<char> {
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
            Self::Copy => '\u{E8C8}',
            Self::Close => '\u{E711}',
            Self::Signal => '\u{E701}',
            Self::Send => '\u{E724}',
            Self::Link => '\u{E71B}',
            Self::Volume => '\u{E767}',
            Self::Info => '\u{E946}',
            Self::Effects => '\u{E790}',
            Self::Hand => '\u{270B}',
            Self::Emoji => '\u{E76E}',
            Self::Key => '\u{E8D7}',
            Self::Folder => '\u{E8B7}',
            Self::Play => '\u{E768}',
            Self::Refresh => '\u{E72C}',
            Self::Mirror | Self::SpeakerView => return None,
        };
        Some(glyph)
    }

    /// The glyph in the bundled Phosphor subset (regular weight). The name in each comment is
    /// Phosphor's, for finding it again.
    fn phosphor(self) -> Option<char> {
        let glyph = match self {
            Self::Mic => '\u{E326}',     // microphone
            Self::MicOff => '\u{E328}',  // microphone-slash
            Self::Video => '\u{E4DA}',   // video-camera
            Self::Share => '\u{E404}',   // screencast
            Self::People => '\u{E4D6}',  // users
            Self::Chat => '\u{E166}',    // chat-centered-text
            Self::Leave => '\u{E3BC}',   // phone-disconnect
            Self::Lock => '\u{E2FA}',    // lock
            Self::Shield => '\u{E40A}',  // shield
            Self::Grid => '\u{E464}',    // squares-four
            Self::Copy => '\u{E1CA}',    // copy
            Self::Close => '\u{E4F6}',   // x
            Self::Signal => '\u{E4EA}',  // wifi-high
            Self::Send => '\u{E396}',    // paper-plane-right
            Self::Link => '\u{E2E2}',    // link
            Self::Volume => '\u{E44A}',  // speaker-high
            Self::Info => '\u{E2CE}',    // info
            Self::Effects => '\u{E6C8}', // palette
            Self::Hand => '\u{E57E}',    // hand-palm
            Self::Emoji => '\u{E436}',   // smiley
            Self::Key => '\u{E2D6}',     // key
            Self::Folder => '\u{E24A}',  // folder
            Self::Play => '\u{E3D0}',    // play
            Self::Refresh => '\u{E036}', // arrow-clockwise
            Self::Mirror => '\u{ED6A}',  // flip-horizontal
            Self::SpeakerView => return None,
        };
        Some(glyph)
    }

    /// The glyph in the icon font in use, or `None` for an icon drawn from shapes.
    pub fn glyph(self) -> Option<char> {
        if SEGOE.load(Ordering::Relaxed) {
            self.segoe()
        } else {
            self.phosphor()
        }
    }

    /// Paints the icon centred at `center`: the font glyph, or shapes for a drawn icon.
    pub fn paint(self, painter: &Painter, center: Pos2, size: f32, colour: Color32) -> Rect {
        if let Some(glyph) = self.glyph() {
            let font = FontId::new(size, FontFamily::Name(ICON_FAMILY.into()));
            return painter.text(center, Align2::CENTER_CENTER, glyph, font, colour);
        }
        painter.extend(self.shapes(center, size, colour));
        Rect::from_center_size(center, vec2(size, size))
    }

    /// The icons no font in use has, as shapes in a `size` square around `center`.
    /// Coordinates are in icon units: -0.5..0.5 each way, y down. Empty for every other icon.
    fn shapes(self, center: Pos2, size: f32, colour: Color32) -> Vec<Shape> {
        let stroke = Stroke::new((size * 0.09).max(1.0), colour);
        let p = |x: f32, y: f32| center + vec2(x, y) * size;
        let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_two_pos(p(x0, y0), p(x1, y1));
        let poly =
            |pts: &[(f32, f32)]| Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke);
        match self {
            // The strip is filled: outlines this small blur at the top bar's icon size.
            Self::SpeakerView => vec![
                Shape::rect_stroke(
                    r(-0.45, -0.4, 0.45, 0.14),
                    0.04 * size,
                    stroke,
                    StrokeKind::Middle,
                ),
                Shape::rect_filled(r(-0.45, 0.26, -0.17, 0.42), 0.0, colour),
                Shape::rect_filled(r(-0.14, 0.26, 0.14, 0.42), 0.0, colour),
                Shape::rect_filled(r(0.17, 0.26, 0.45, 0.42), 0.0, colour),
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
                Shape::closed_line(vec![p(0.13, -0.3), p(0.13, 0.3), p(0.45, 0.3)], stroke),
            ],
            _ => Vec::new(),
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
    use std::sync::Arc;

    use eframe::egui::{self, FontData, FontDefinitions};

    use super::*;

    const ALL: [Icon; 26] = [
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
        Icon::SpeakerView,
        Icon::Copy,
        Icon::Close,
        Icon::Signal,
        Icon::Send,
        Icon::Link,
        Icon::Volume,
        Icon::Info,
        Icon::Effects,
        Icon::Hand,
        Icon::Emoji,
        Icon::Mirror,
        Icon::Key,
        Icon::Folder,
        Icon::Play,
        Icon::Refresh,
    ];

    #[test]
    fn every_phosphor_glyph_is_in_the_bundled_subset() {
        let mut fonts = FontDefinitions::empty();
        fonts.font_data.insert(
            "phosphor".into(),
            Arc::new(FontData::from_static(PHOSPHOR_TTF)),
        );
        fonts
            .families
            .insert(FontFamily::Proportional, vec!["phosphor".into()]);
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        // No renderer here to apply the font atlas upload; egui asserts it was handled.
        output.textures_delta.clear();
        let font = FontId::proportional(16.0);
        for icon in ALL {
            if let Some(glyph) = icon.phosphor() {
                assert!(
                    ctx.fonts_mut(|f| f.has_glyph(&font, glyph)),
                    "{icon:?} ({glyph:?}) is missing from PhosphorVeil.ttf"
                );
            }
        }
    }

    #[test]
    fn every_icon_has_a_glyph_or_shapes_in_both_fonts() {
        let center = Pos2::new(100.0, 100.0);
        for icon in ALL {
            let drawn = !icon.shapes(center, 20.0, Color32::WHITE).is_empty();
            assert!(icon.segoe().is_some() || drawn, "{icon:?} blank on Windows");
            assert!(
                icon.phosphor().is_some() || drawn,
                "{icon:?} blank elsewhere"
            );
        }
    }

    #[test]
    fn drawn_icons_stay_inside_their_square() {
        let size = 20.0;
        let center = Pos2::new(100.0, 100.0);
        // Half a stroke may spill past the edge.
        let square = Rect::from_center_size(center, vec2(size, size)).expand(size * 0.05);
        for icon in ALL {
            for shape in icon.shapes(center, size, Color32::WHITE) {
                let bounds = shape.visual_bounding_rect();
                assert!(
                    square.contains_rect(bounds),
                    "{icon:?} spills out: {bounds:?}"
                );
            }
        }
    }
}
