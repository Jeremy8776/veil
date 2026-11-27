//! Chat text laid out so emoji draw from the colour emoji font.
//!
//! Purpose: egui picks, for each character, the first font in a family that has it. Linux UI
//! fonts (DejaVu Sans, Noto Sans) and Android's Roboto carry one-colour copies of many emoji
//! (❤ ✨ ☕ ✅), so a family that starts with the UI font draws those in one colour beside
//! colour ones. A family that starts with the emoji font cannot be used for all text either:
//! colour emoji fonts also hold digits, `#` and `*` for keycaps.
//!
//! So chat text is split into runs. Emoji runs use [`theme::emoji_family`] (emoji font
//! first), everything else uses [`theme::chat_family`].
//!
//! Invariants:
//! - Digits, letters and punctuation are never in an emoji run unless an emoji presentation
//!   selector (U+FE0F) or keycap mark (U+20E3) follows them, as in "1️⃣" or "©️".
//! - Joiners, selectors and skin tones stay in the run of the emoji they modify.
//! - A character followed by the text presentation selector (U+FE0E) is text, with the
//!   selector in its run, so "❤︎" draws in one colour as the sender chose.

use std::ops::Range;

use eframe::egui::{Color32, FontId, text::LayoutJob, text::TextFormat};

use crate::theme;

/// Emoji presentation selector: the character before it is drawn as emoji.
const EMOJI_SELECTOR: char = '\u{FE0F}';
/// Text presentation selector: the character before it is drawn as text, as in "❤︎".
const TEXT_SELECTOR: char = '\u{FE0E}';
/// Combining keycap: "1" + U+FE0F + U+20E3 is the keycap emoji.
const KEYCAP: char = '\u{20E3}';

/// Characters drawn from the emoji font when it has them.
fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0x1F000..=0x1FAFF   // pictographs, faces, flags, skin tones
            | 0x2300..=0x23FF // ⌚ ⏰ ⏳
            | 0x2600..=0x27BF // ☕ ❤ ✨ ✅
            | 0x2B00..=0x2BFF // ⭐ ⬛
            | 0x3030 | 0x303D | 0x3297 | 0x3299
            | 0x200D            // zero-width joiner
            | 0xFE0F | 0x20E3
            | 0xE0020..=0xE007F // tags, for subdivision flags
    )
}

/// `text` split into byte ranges, each marked `true` when it is an emoji run.
fn runs(text: &str) -> Vec<(Range<usize>, bool)> {
    let mut out: Vec<(Range<usize>, bool)> = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let emoji = match next {
            Some(TEXT_SELECTOR) => false,
            Some(EMOJI_SELECTOR | KEYCAP) => true,
            _ => is_emoji(c),
        };
        let end = at + c.len_utf8();
        match out.last_mut() {
            Some((range, was)) if *was == emoji => range.end = end,
            _ => out.push((at..end, emoji)),
        }
    }
    out
}

/// `text` at `size` in `color`, with emoji runs in the emoji family. Pass
/// [`Color32::PLACEHOLDER`] to let the widget choose the colour.
pub fn job(text: &str, size: f32, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (range, emoji) in runs(text) {
        let family = if emoji {
            theme::emoji_family()
        } else {
            theme::chat_family()
        };
        let format = TextFormat::simple(FontId::new(size, family), color);
        job.append(&text[range], 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<(&str, bool)> {
        runs(text)
            .into_iter()
            .map(|(range, emoji)| (&text[range], emoji))
            .collect()
    }

    #[test]
    fn letters_and_emoji_form_separate_runs() {
        assert_eq!(
            split("hi ❤ there ☕✨"),
            vec![
                ("hi ", false),
                ("❤", true),
                (" there ", false),
                ("☕✨", true)
            ]
        );
    }

    #[test]
    fn digits_stay_text_unless_a_keycap_follows() {
        assert_eq!(split("call 2 at 10#"), vec![("call 2 at 10#", false)]);
        assert_eq!(
            split("press 1\u{FE0F}\u{20E3} now"),
            vec![
                ("press ", false),
                ("1\u{FE0F}\u{20E3}", true),
                (" now", false)
            ]
        );
    }

    #[test]
    fn joined_and_toned_emoji_stay_in_one_run() {
        let family = "👩\u{200D}👩\u{200D}👧";
        let wave = "👋\u{1F3FD}";
        assert_eq!(split(family), vec![(family, true)]);
        assert_eq!(split(wave), vec![(wave, true)]);
        assert_eq!(split("©\u{FE0F}"), vec![("©\u{FE0F}", true)]);
    }

    #[test]
    fn every_offered_emoji_is_one_emoji_run() {
        for emoji in crate::chat::PICKER
            .iter()
            .chain(crate::chat::QUICK_REACTIONS.iter())
        {
            assert_eq!(split(emoji), vec![(*emoji, true)], "{emoji}");
        }
    }

    #[test]
    fn a_text_selector_keeps_the_character_as_text() {
        assert_eq!(
            split("I \u{2764}\u{FE0E} it \u{2764}"),
            vec![("I \u{2764}\u{FE0E} it ", false), ("\u{2764}", true)]
        );
    }

    #[test]
    fn empty_text_has_no_runs() {
        assert!(runs("").is_empty());
    }
}
