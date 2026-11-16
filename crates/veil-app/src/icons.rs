//! Icons from the system icon font (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on
//! Windows 10), with a text fallback.
//!
//! Purpose: one enum for every icon the app shows, so a codepoint is written once. The
//! codepoints were checked against both fonts' character maps and by rendering them.
//!
//! Invariants:
//! - Nothing here is bundled; the font belongs to the operating system.
//! - Without an icon font, [`Icon::text`] returns a short word instead, so every control
//!   still says what it does.
//! - An "off" state (camera off) is the "on" icon with a slash painted across, not a
//!   separate glyph, so it looks the same in both fonts.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{Color32, FontFamily, FontId, Painter, Pos2, Rect, Stroke, vec2};

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
    /// A raised hand, from the symbol font (the icon fonts have none).
    Hand,
}

impl Icon {
    fn codepoint(self) -> char {
        match self {
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
            Self::Hand => '\u{270B}',
        }
    }

    fn fallback(self) -> &'static str {
        match self {
            Self::Mic => "Mic",
            Self::MicOff => "Muted",
            Self::Video => "Cam",
            Self::Share => "Share",
            Self::People => "People",
            Self::Chat => "Chat",
            Self::Leave => "Leave",
            Self::Lock => "E2E",
            Self::Shield => "Safe",
            Self::Grid => "Grid",
            Self::Speaker => "Focus",
            Self::Copy => "Copy",
            Self::Close => "X",
            Self::Signal => "Net",
            Self::Send => "Send",
            Self::Link => "Link",
            Self::Volume => "Spk",
            Self::Info => "i",
            Self::Hand => "Hand",
        }
    }

    /// The glyph, or a short word without the icon font.
    pub fn text(self) -> String {
        if HAVE_FONT.load(Ordering::Relaxed) {
            self.codepoint().to_string()
        } else {
            self.fallback().to_string()
        }
    }

    /// The font to draw [`Icon::text`] with.
    pub fn font(size: f32) -> FontId {
        if HAVE_FONT.load(Ordering::Relaxed) {
            FontId::new(size, FontFamily::Name(ICON_FAMILY.into()))
        } else {
            FontId::proportional(size * 0.6)
        }
    }

    /// Paints the icon centred at `center`.
    pub fn paint(self, painter: &Painter, center: Pos2, size: f32, colour: Color32) -> Rect {
        painter.text(
            center,
            eframe::egui::Align2::CENTER_CENTER,
            self.text(),
            Self::font(size),
            colour,
        )
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
