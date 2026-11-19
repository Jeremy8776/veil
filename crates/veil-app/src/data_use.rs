//! How much data a call uses: Auto, Data saver or Audio only.
//!
//! Purpose: the person picks one on the pre-join screen or from the camera menu during a
//! call. The choice is saved with the connection settings (`connection.rs`) and shared with
//! the call's threads through a [`DataUseCell`], so a change mid-call applies within about a
//! second, with no new call.
//! - The camera encoder (`engine/video_out.rs`) picks its ladder from it, and sends nothing
//!   in Audio only.
//! - A shared screen is capped in Data saver and Audio only (`video_out::screen_ladder`).
//! - Opus runs at [`DataUse::opus_bitrate`] (`engine/session.rs`).
//! - Through a forwarder, the layers asked for are capped at [`DataUse::wanted_cap`]
//!   (`engine/wanted.rs`).
//!
//! Invariants:
//! - [`DataUse::Auto`] is the default and behaves exactly as Veil did before this choice.
//! - Sound is never turned off by this choice; only video and the sound bitrate change.
//!
//! shortcut: Auto does not yet step down by itself when the link is poor. That needs a
//! bandwidth estimate per link (ARCHITECTURE.md 3.3, item 8); add it there and let Auto
//! read it.
//! shortcut: in a mesh call, others still send you every layer they encode, so Data saver and
//! Audio only lower what you send, not what you receive. Through a forwarder you receive at
//! most the smallest layer. Stopping incoming video entirely needs a "no video" request in
//! the forwarder protocol and a per-link request in the mesh.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};
use veil_forward::Wanted;

/// How much data a call may use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataUse {
    /// The best picture the connection allows. Recommended.
    #[default]
    Auto,
    /// A small, slow picture and lower-rate sound.
    Saver,
    /// No video from you, lower-rate sound.
    AudioOnly,
}

/// Opus rate for Auto: the speech default in `veil_media::AudioSettings::voice`.
const FULL_OPUS: u32 = 32_000;
/// Opus rate for Data saver and Audio only. Speech stays clear well below this.
const LOW_OPUS: u32 = 16_000;

impl DataUse {
    /// In the order the menus show them, the recommended one first.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Saver, Self::AudioOnly];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (recommended)",
            Self::Saver => "Data saver",
            Self::AudioOnly => "Audio only",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Auto => {
                "The best picture your connection allows. Right for most home, office and \
                 mobile connections."
            }
            Self::Saver => {
                "A small picture at a low frame rate, and lower-rate sound. For mobile data \
                 or a slow or costly connection. Your video uses about 150 kbit/s."
            }
            Self::AudioOnly => {
                "Turns your camera off and uses lower-rate sound. For very slow networks such \
                 as 2G. Others' video still reaches you, at its smallest size when the call \
                 uses a forwarder."
            }
        }
    }

    /// Whether your camera is encoded and sent.
    pub fn sends_camera(self) -> bool {
        self != Self::AudioOnly
    }

    /// The Opus bitrate for your sound, in bit/s.
    pub fn opus_bitrate(self) -> u32 {
        match self {
            Self::Auto => FULL_OPUS,
            Self::Saver | Self::AudioOnly => LOW_OPUS,
        }
    }

    /// The most to ask a forwarder for from each sender, or `None` for whatever the tile
    /// needs.
    pub fn wanted_cap(self) -> Option<Wanted> {
        match self {
            Self::Auto => None,
            Self::Saver => Some(crate::engine::wanted::SMALL),
            Self::AudioOnly => Some(Wanted::THUMB),
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Saver => 1,
            Self::AudioOnly => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Saver,
            2 => Self::AudioOnly,
            _ => Self::Auto,
        }
    }
}

/// The current choice, shared by the interface and the call's threads.
#[derive(Debug, Default)]
pub struct DataUseCell(AtomicU8);

impl DataUseCell {
    pub fn new(data_use: DataUse) -> Self {
        Self(AtomicU8::new(data_use.to_u8()))
    }

    pub fn get(&self) -> DataUse {
        DataUse::from_u8(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, data_use: DataUse) {
        self.0.store(data_use.to_u8(), Ordering::Relaxed);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_the_default_and_listed_first() {
        assert_eq!(DataUse::default(), DataUse::Auto);
        assert_eq!(DataUse::ALL[0], DataUse::Auto);
        assert!(DataUse::Auto.label().contains("recommended"));
        assert_eq!(DataUse::Auto.wanted_cap(), None);
        assert_eq!(DataUse::Auto.opus_bitrate(), 32_000);
    }

    #[test]
    fn the_cell_keeps_each_choice() {
        let cell = DataUseCell::default();
        assert_eq!(cell.get(), DataUse::Auto);
        for data_use in DataUse::ALL {
            cell.set(data_use);
            assert_eq!(cell.get(), data_use);
        }
    }

    #[test]
    fn lower_choices_send_less() {
        assert!(DataUse::Saver.sends_camera());
        assert!(!DataUse::AudioOnly.sends_camera());
        for low in [DataUse::Saver, DataUse::AudioOnly] {
            assert!(low.opus_bitrate() < DataUse::Auto.opus_bitrate());
            assert!(low.opus_bitrate() >= 6_000, "Opus refuses less than 6 kbit/s");
        }
        assert_eq!(DataUse::AudioOnly.wanted_cap(), Some(Wanted::THUMB));
    }

    #[test]
    fn saved_names_are_stable() {
        let text = serde_json::to_string(&DataUse::AudioOnly).unwrap();
        assert_eq!(text, "\"audio_only\"");
        assert_eq!(serde_json::from_str::<DataUse>("\"saver\"").unwrap(), DataUse::Saver);
    }
}
