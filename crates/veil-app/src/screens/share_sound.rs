//! The sound controls of a shared screen, in a small strip over its top right corner.
//!
//! Purpose: on your own share, while it includes computer sound, the strip says so and gives
//! your level and mute, which change what everyone hears. On someone else's share, while its
//! sound is arriving, it gives your own volume and mute for it, which change only what you
//! hear (`volume::ShareLevel`).
//!
//! Invariants:
//! - Nothing is drawn on a share without sound, or on a picture too small to hold the strip.
//! - Changes apply to the next 20 ms frame of sound.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Rect, RichText, UiBuilder, pos2, vec2,
};

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::screens::call_menus;
use crate::share::SoundState;
use crate::theme::{ACCENT, MUTED, TEXT};
use crate::volume::{SPEAKER_MAX, ShareLevel};
use crate::widgets::{Tone, pill};

const STRIP: egui::Vec2 = vec2(330.0, 40.0);
const MARGIN: f32 = 8.0;

/// Whose share a stage shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    You,
    /// Someone else, and whether their share's sound is arriving.
    Them {
        id: u32,
        sounding: bool,
    },
}

/// The strip's place in a shared picture's `rect`, if the picture is large enough for it.
pub fn strip_rect(rect: Rect) -> Option<Rect> {
    let fits = rect.width() >= STRIP.x + 2.0 * MARGIN && rect.height() >= 3.0 * STRIP.y;
    fits.then(|| {
        Rect::from_min_size(
            pos2(rect.right() - STRIP.x - MARGIN, rect.top() + MARGIN),
            STRIP,
        )
    })
}

/// Draws the sound controls over the shared picture in `rect`.
pub fn overlay(ui: &mut egui::Ui, rect: Rect, app: &VeilApp, owner: Owner) {
    let sound = app.share.sound.state();
    let shown = match owner {
        Owner::You => sound.is_on(),
        Owner::Them { sounding, .. } => sounding,
    };
    let Some(strip) = strip_rect(rect).filter(|_| shown) else {
        return;
    };
    ui.painter()
        .rect_filled(strip, CornerRadius::same(8), Color32::from_black_alpha(190));
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(("share-sound", owner_key(owner)))
            .max_rect(strip.shrink2(vec2(8.0, 4.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    match owner {
        Owner::You => yours(&mut child, &sound),
        Owner::Them { id, .. } => theirs(&mut child, app, id),
    }
}

fn owner_key(owner: Owner) -> u64 {
    match owner {
        Owner::You => u64::MAX,
        Owner::Them { id, .. } => u64::from(id),
    }
}

/// Your share: says sound is going out, with your level and mute for everyone.
fn yours(ui: &mut egui::Ui, sound: &SoundState) {
    let muted = sound.muted();
    let (label, colour) = if muted {
        ("Sound muted", MUTED)
    } else {
        ("Sending sound", ACCENT)
    };
    ui.label(RichText::new(label).small().color(colour))
        .on_hover_text("Your computer's sound goes out with your share. Call sound is left out.");
    let button = if muted { "Unmute" } else { "Mute" };
    let hint = if muted {
        "Send your computer's sound again"
    } else {
        "Stop sending your computer's sound. Your screen stays shared."
    };
    if pill(ui, Some(Icon::Volume), button, Tone::Quiet, 92.0)
        .on_hover_text(hint)
        .clicked()
    {
        sound.set_muted(!muted);
    }
    let mut level = sound.level();
    let hint = "How loud your computer's sound is for everyone";
    if call_menus::volume(ui, "Share sound level", &mut level, SPEAKER_MAX, hint) {
        sound.set_level(level);
    }
}

/// Someone else's share: your own volume and mute for its sound.
fn theirs(ui: &mut egui::Ui, app: &VeilApp, id: u32) {
    let playback = app.media.playback();
    let level = playback.share_level(id);
    ui.label(RichText::new("Shared sound").small().color(TEXT))
        .on_hover_text("Sound from this share. Your settings change only what you hear.");
    let button = if level.muted { "Unmute" } else { "Mute" };
    if pill(ui, Some(Icon::Volume), button, Tone::Quiet, 92.0)
        .on_hover_text("Only for you")
        .clicked()
    {
        playback.set_share_level(
            id,
            ShareLevel {
                muted: !level.muted,
                ..level
            },
        );
    }
    let mut volume = level.volume;
    let hint = "How loud this share plays for you";
    if call_menus::volume(ui, "Share volume", &mut volume, SPEAKER_MAX, hint) {
        playback.set_share_level(id, ShareLevel { volume, ..level });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_sits_top_right_and_only_on_a_large_enough_picture() {
        let big = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 450.0));
        let strip = strip_rect(big).map(|r| (r.right(), r.top()));
        assert_eq!(strip, Some((792.0, 8.0)));
        let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 170.0));
        assert!(strip_rect(small).is_none());
    }
}
