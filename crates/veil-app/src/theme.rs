//! Veil's look: a dark palette, spacing, and the fonts, loaded from the operating system.
//!
//! Purpose: one place for every colour and size, so screens never pick their own.
//!
//! Invariants:
//! - Text fonts are never bundled. egui's bundled fonts carry licences outside Veil's allow
//!   list (`tools/licences.py`), so the app reads the system's UI font at start-up. Without a
//!   UI font the app cannot draw text, so that is a start-up error.
//! - Icons come from the system's Segoe icon font on Windows. Elsewhere Veil uses its bundled
//!   Phosphor subset ([`crate::icons::PHOSPHOR_TTF`], MIT), so there is always an icon font.
//! - The emoji font is optional too. In the UI it comes after the UI and symbol fonts, so it
//!   only draws what they lack. Chat uses [`chat_family`], where it comes before the symbol
//!   font: Segoe UI Symbol has one-colour copies of most emoji, which would otherwise win.

use std::sync::Arc;

use crate::system_fonts;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Visuals,
};

/// The page behind everything.
pub const PAGE: Color32 = Color32::from_rgb(0x17, 0x16, 0x1C);
/// Panels and bars.
pub const SURFACE: Color32 = Color32::from_rgb(0x22, 0x21, 0x29);
/// Cards, inputs and idle buttons on a surface.
pub const RAISED: Color32 = Color32::from_rgb(0x2E, 0x2D, 0x37);
/// Hovered raised items.
pub const RAISED_HOVER: Color32 = Color32::from_rgb(0x3A, 0x39, 0x45);
/// Video tiles with no picture.
pub const TILE: Color32 = Color32::from_rgb(0x29, 0x28, 0x31);
pub const LINE: Color32 = Color32::from_rgb(0x3D, 0x3B, 0x47);
pub const TEXT: Color32 = Color32::from_rgb(0xF3, 0xF2, 0xF7);
pub const MUTED: Color32 = Color32::from_rgb(0xA7, 0xA5, 0xB4);
/// Veil's accent, for the main action and selected states.
pub const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x6A, 0xF0);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x86, 0x83, 0xF5);
/// Leave, decline, and a muted microphone.
pub const DANGER: Color32 = Color32::from_rgb(0xD1, 0x34, 0x38);
pub const DANGER_HOVER: Color32 = Color32::from_rgb(0xE0, 0x4A, 0x4E);
/// The ring round whoever is speaking, and good connection states.
pub const SPEAKING: Color32 = Color32::from_rgb(0x4C, 0xC3, 0x8A);
/// Warnings: demo mode, relayed connections.
pub const WARN: Color32 = Color32::from_rgb(0xE8, 0xB3, 0x4B);
/// The logo's colour: plain white, as in `tools/make-icons.py`.
pub const LOGO: Color32 = Color32::WHITE;

pub const RADIUS: u8 = 8;
pub const GAP: f32 = 8.0;

/// The icon font family name.
pub const ICON_FAMILY: &str = "icons";

/// Where each operating system usually keeps a UI font, best first. Linux distributions vary,
/// so [`crate::system_fonts`] searches their font folders when these are missing.
const UI_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    "/System/Library/Fonts/SFNS.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];
/// Semibold, for headings and names, if present.
const BOLD_FONTS: &[&str] = &[r"C:\Windows\Fonts\seguisb.ttf"];
/// Symbols the UI font lacks (the raised hand).
const SYMBOL_FONTS: &[&str] = &[r"C:\Windows\Fonts\seguisym.ttf"];
/// Colour emoji for chat, drawn by epaint's `color_fonts`: Segoe UI Emoji (COLR) on Windows,
/// Apple Color Emoji (sbix, a collection read at face 0) on macOS. Linux distributions vary,
/// so [`system_fonts::EMOJI_FONT_FILES`] are searched there (Noto Color Emoji is CBDT).
const EMOJI_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\seguiemj.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
];
/// System icon fonts: Windows 11, then Windows 10. Without one, the bundled Phosphor is used.
const ICON_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\SegoeIcons.ttf",
    r"C:\Windows\Fonts\segmdl2.ttf",
];

/// What [`load_fonts`] found.
pub struct FontsFound {
    /// The system's Segoe icon font loaded, rather than the bundled Phosphor.
    pub segoe_icons: bool,
}

/// Loads the system fonts into egui. Fails only when there is no UI font at all.
pub fn load_fonts(ctx: &egui::Context) -> Result<FontsFound, String> {
    let mut fonts = FontDefinitions::empty();
    let ui = first_readable(UI_FONTS)
        .or_else(|| system_fonts::find_font(system_fonts::UI_FONT_FILES))
        .ok_or("no system UI font found")?;
    fonts
        .font_data
        .insert("ui".into(), Arc::new(FontData::from_owned(ui)));
    let mut proportional = vec![String::from("ui")];
    if let Some(symbols) = first_readable(SYMBOL_FONTS) {
        fonts
            .font_data
            .insert("symbols".into(), Arc::new(FontData::from_owned(symbols)));
        proportional.push("symbols".into());
    }
    let emoji = first_readable(EMOJI_FONTS)
        .or_else(|| system_fonts::find_font(system_fonts::EMOJI_FONT_FILES));
    // Chat: the UI font for letters, then colour emoji ahead of the symbol font.
    let mut chat = vec![String::from("ui")];
    if let Some(emoji) = emoji {
        fonts
            .font_data
            .insert("emoji".into(), Arc::new(FontData::from_owned(emoji)));
        proportional.push("emoji".into());
        chat.push("emoji".into());
    }
    chat.extend(
        proportional
            .iter()
            .skip(1)
            .filter(|f| *f != "emoji")
            .cloned(),
    );
    fonts.families.insert(chat_family(), chat);
    let bold = first_readable(BOLD_FONTS);
    let mut bold_family = proportional.clone();
    if let Some(bold) = bold {
        fonts
            .font_data
            .insert("bold".into(), Arc::new(FontData::from_owned(bold)));
        bold_family.insert(0, "bold".into());
    }
    fonts
        .families
        .insert(FontFamily::Proportional, proportional.clone());
    fonts
        .families
        .insert(FontFamily::Monospace, proportional.clone());
    fonts
        .families
        .insert(FontFamily::Name("bold".into()), bold_family);
    let segoe = first_readable(ICON_FONTS);
    let found = FontsFound {
        segoe_icons: segoe.is_some(),
    };
    let icons = match segoe {
        Some(segoe) => FontData::from_owned(segoe),
        None => FontData::from_static(crate::icons::PHOSPHOR_TTF),
    };
    fonts.font_data.insert(ICON_FAMILY.into(), Arc::new(icons));
    // Icons, then the UI and symbol fonts for anything the icon font lacks (the hand on
    // Windows).
    let mut family = vec![String::from(ICON_FAMILY)];
    family.extend(proportional.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(ICON_FAMILY.into()), family);
    ctx.set_fonts(fonts);
    Ok(found)
}

fn first_readable(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|path| std::fs::read(path).ok())
}

/// The font family for chat text and emoji: colour emoji win over one-colour symbols.
pub fn chat_family() -> FontFamily {
    FontFamily::Name("chat".into())
}

/// The chat font at the body text size of `ui`.
pub fn chat_font(ui: &egui::Ui) -> FontId {
    FontId::new(TextStyle::Body.resolve(ui.style()).size, chat_family())
}

/// A semibold font of `size`, or regular where no semibold was found.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// Dark visuals and Veil's spacing.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = SURFACE;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = RAISED;
    visuals.faint_bg_color = RAISED;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT_HOVER;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, TEXT);
    let radius = CornerRadius::same(RADIUS);
    for (widget, fill) in [
        (&mut visuals.widgets.noninteractive, SURFACE),
        (&mut visuals.widgets.inactive, RAISED),
        (&mut visuals.widgets.hovered, RAISED_HOVER),
        (&mut visuals.widgets.active, LINE),
        (&mut visuals.widgets.open, RAISED_HOVER),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.corner_radius = radius;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke = Stroke::new(1.0, TEXT);
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, LINE);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(GAP, GAP);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.window_margin = Margin::same(16);
        style.spacing.interact_size.y = 34.0;
        for (text, size) in [
            (TextStyle::Heading, 22.0),
            (TextStyle::Body, 14.0),
            (TextStyle::Button, 14.0),
            (TextStyle::Small, 12.0),
            (TextStyle::Monospace, 14.0),
        ] {
            if let Some(font) = style.text_styles.get_mut(&text) {
                font.size = size;
            }
        }
        if let Some(heading) = style.text_styles.get_mut(&TextStyle::Heading) {
            heading.family = FontFamily::Name("bold".into());
        }
    });
}

/// A stable colour for someone's avatar, from their name.
pub fn avatar_colour(name: &str) -> Color32 {
    const COLOURS: [Color32; 8] = [
        Color32::from_rgb(0x5B, 0x5F, 0xC7),
        Color32::from_rgb(0x2E, 0x8B, 0x7A),
        Color32::from_rgb(0xB2, 0x5C, 0x2E),
        Color32::from_rgb(0x8A, 0x4B, 0xAF),
        Color32::from_rgb(0x2F, 0x7B, 0xBF),
        Color32::from_rgb(0xA8, 0x3E, 0x6B),
        Color32::from_rgb(0x5E, 0x8C, 0x31),
        Color32::from_rgb(0x9C, 0x7A, 0x22),
    ];
    let hash = name
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    COLOURS[hash as usize % COLOURS.len()]
}

/// Up to two initials: first letters of the first and last words.
pub fn initials(name: &str) -> String {
    let mut words = name.split_whitespace();
    let first = words.next().and_then(|w| w.chars().next());
    let last = words.last().and_then(|w| w.chars().next());
    first
        .into_iter()
        .chain(last)
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_first_and_last_words() {
        assert_eq!(initials("Amara Okafor"), "AO");
        assert_eq!(initials("lucía de la Fuente"), "LF");
        assert_eq!(initials("Sam"), "S");
        assert_eq!(initials("  "), "");
    }

    #[test]
    fn avatar_colours_are_stable() {
        assert_eq!(avatar_colour("Kenji"), avatar_colour("Kenji"));
    }
}
