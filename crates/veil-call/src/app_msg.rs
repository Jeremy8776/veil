//! App messages between members: each member's display name, and chat.
//!
//! Purpose: names and chat ride the media links next to video and sound, sealed with the
//! sender's frame key for the current epoch ([`AppMessage::seal`]), so relays and forwarders
//! see only ciphertext and its size. The receiver opens the packet with its `KeyRing`, which
//! gives the authenticated sender index (MLS leaf), then calls [`AppMessage::decode`]. Who
//! sent a message is that index, never the link it came on.
//!
//! Wire format (the plaintext inside the sealed frame):
//! - 8 bytes `veil-app`, then a version byte (1). Video and sound never start like this, so
//!   a forwarder that moves a sealed frame from one channel to another gets it refused here.
//! - a postcard-encoded [`AppMessage`], with nothing after it.
//!
//! Invariants:
//! - Every message is checked on the way in and out: names 1 to [`MAX_NAME_CHARS`]
//!   characters, chat 1 to [`MAX_CHAT_CHARS`], no control characters (chat may hold line
//!   breaks and tabs) and no bidirectional overrides, which could make a name read as another.
//! - A sealed message is at most `veil_net::control::MAX_APP_LEN` bytes; larger plaintext is
//!   refused before decoding.
//! - Replays are refused by the frame key's replay window, shared with video and sound.

use std::fmt;

use serde::{Deserialize, Serialize};
use veil_net::control::MAX_APP_LEN;
use veil_security::frame::{FrameSender, HEADER_LEN, TAG_LEN};

/// The longest display name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// The longest chat message, in characters.
pub const MAX_CHAT_CHARS: usize = 2_000;

const MAGIC: &[u8; 8] = b"veil-app";
const VERSION: u8 = 1;
const PREFIX_LEN: usize = MAGIC.len() + 1;
/// The largest plaintext that still seals within `MAX_APP_LEN`.
const MAX_PLAIN_LEN: usize = MAX_APP_LEN - HEADER_LEN - TAG_LEN;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum AppMessage {
    /// The sender's display name. Sent on joining and after each epoch change.
    Name(String),
    /// One chat message to everyone in the call.
    Chat(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    Empty,
    TooLong,
    /// A control character or a bidirectional override.
    BadCharacter,
    /// Not an app message, an unknown version, or trailing bytes.
    Malformed,
    /// The frame key refused to seal (its counter ran out).
    Seal,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty message",
            Self::TooLong => "message too long",
            Self::BadCharacter => "message holds a character that is not allowed",
            Self::Malformed => "not a valid app message",
            Self::Seal => "cannot seal the message",
        })
    }
}

impl std::error::Error for AppError {}

impl AppMessage {
    /// A name message from what the person typed, trimmed.
    pub fn name(text: &str) -> Result<Self, AppError> {
        let message = Self::Name(text.trim().to_string());
        message.check()?;
        Ok(message)
    }

    /// A chat message from what the person typed, trimmed.
    pub fn chat(text: &str) -> Result<Self, AppError> {
        let message = Self::Chat(text.trim().to_string());
        message.check()?;
        Ok(message)
    }

    /// Checks the length and characters of the text.
    pub fn check(&self) -> Result<(), AppError> {
        let (text, max, allow_breaks) = match self {
            Self::Name(text) => (text, MAX_NAME_CHARS, false),
            Self::Chat(text) => (text, MAX_CHAT_CHARS, true),
        };
        if text.trim().is_empty() {
            return Err(AppError::Empty);
        }
        if text.chars().count() > max {
            return Err(AppError::TooLong);
        }
        let allowed = |c: char| allow_breaks && (c == '\n' || c == '\t');
        if text
            .chars()
            .any(|c| (c.is_control() && !allowed(c)) || is_bidi_control(c))
        {
            return Err(AppError::BadCharacter);
        }
        Ok(())
    }

    /// The plaintext to seal: the prefix, then the message.
    pub fn encode(&self) -> Result<Vec<u8>, AppError> {
        self.check()?;
        let mut out = Vec::with_capacity(PREFIX_LEN + 64);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        let out = postcard::to_extend(self, out).map_err(|_| AppError::Malformed)?;
        if out.len() > MAX_PLAIN_LEN {
            return Err(AppError::TooLong);
        }
        Ok(out)
    }

    /// Reads an opened app message and checks it.
    pub fn decode(plain: &[u8]) -> Result<Self, AppError> {
        if plain.len() > MAX_PLAIN_LEN {
            return Err(AppError::TooLong);
        }
        let body = plain
            .strip_prefix(MAGIC.as_slice())
            .and_then(|rest| rest.strip_prefix(&[VERSION]))
            .ok_or(AppError::Malformed)?;
        let (message, rest): (Self, _) =
            postcard::take_from_bytes(body).map_err(|_| AppError::Malformed)?;
        if !rest.is_empty() {
            return Err(AppError::Malformed);
        }
        message.check()?;
        Ok(message)
    }

    /// Encodes and seals with this epoch's frame key, ready for `MediaSession::send_app`.
    pub fn seal(&self, sender: &mut FrameSender) -> Result<Vec<u8>, AppError> {
        let plain = self.encode()?;
        sender.encrypt(&plain).map_err(|_| AppError::Seal)
    }
}

/// Characters that reorder text around them (U+202A to U+202E, U+2066 to U+2069) and the
/// marks U+200E, U+200F and U+061C.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_security::frame::FrameReceiver;

    use super::*;

    #[test]
    fn names_and_chat_round_trip() {
        for message in [
            AppMessage::name("  Jo Bloggs ").unwrap(),
            AppMessage::chat("Hello\nsecond line\twith a tab, and émoji 🎉").unwrap(),
        ] {
            let plain = message.encode().unwrap();
            assert!(plain.starts_with(b"veil-app\x01"));
            assert_eq!(AppMessage::decode(&plain).unwrap(), message);
        }
        assert_eq!(
            AppMessage::name(" Jo ").unwrap(),
            AppMessage::Name("Jo".into())
        );
    }

    #[test]
    fn bad_text_is_refused_both_ways() {
        assert_eq!(AppMessage::name("   "), Err(AppError::Empty));
        assert_eq!(AppMessage::chat(""), Err(AppError::Empty));
        assert_eq!(AppMessage::name(&"a".repeat(65)), Err(AppError::TooLong));
        assert!(
            AppMessage::name(&"é".repeat(64)).is_ok(),
            "counts characters, not bytes"
        );
        assert_eq!(AppMessage::chat(&"a".repeat(2_001)), Err(AppError::TooLong));
        assert_eq!(AppMessage::name("Jo\nBloggs"), Err(AppError::BadCharacter));
        assert_eq!(
            AppMessage::name("Jo\u{202E}sggolB"),
            Err(AppError::BadCharacter)
        );
        assert_eq!(AppMessage::chat("bell\u{7}"), Err(AppError::BadCharacter));

        // A sender that skips the checks is still refused on the way in.
        let mut plain = b"veil-app\x01".to_vec();
        plain = postcard::to_extend(&AppMessage::Name("a\u{0}b".into()), plain).unwrap();
        assert_eq!(AppMessage::decode(&plain), Err(AppError::BadCharacter));
        let mut long = b"veil-app\x01".to_vec();
        long = postcard::to_extend(&AppMessage::Chat("a".repeat(3_000)), long).unwrap();
        assert_eq!(AppMessage::decode(&long), Err(AppError::TooLong));
    }

    #[test]
    fn other_bytes_are_not_app_messages() {
        let good = AppMessage::chat("hi").unwrap().encode().unwrap();
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(AppMessage::decode(&trailing), Err(AppError::Malformed));
        let mut version = good.clone();
        version[8] = 2;
        assert_eq!(AppMessage::decode(&version), Err(AppError::Malformed));
        assert_eq!(AppMessage::decode(&good[9..]), Err(AppError::Malformed));
        assert_eq!(
            AppMessage::decode(&[0x00, 0x00, 0x00, 0x01, 0x67]),
            Err(AppError::Malformed)
        );
        assert_eq!(
            AppMessage::decode(&vec![0; MAX_PLAIN_LEN + 1]),
            Err(AppError::TooLong)
        );
    }

    #[test]
    fn sealed_messages_open_only_with_the_epoch_key_and_once() {
        let secret = [7u8; 32];
        let mut sender = FrameSender::new(&secret, 3, 1).unwrap();
        let mut receiver = FrameReceiver::new(&secret, 3).unwrap();
        let message = AppMessage::chat(&"x".repeat(MAX_CHAT_CHARS)).unwrap();
        let sealed = message.seal(&mut sender).unwrap();
        assert!(sealed.len() <= MAX_APP_LEN);
        assert!(
            !sealed.windows(8).any(|w| w == b"veil-app"),
            "nothing readable on the wire"
        );
        let (leaf, plain) = receiver.decrypt(&sealed).unwrap();
        assert_eq!(leaf, 1);
        assert_eq!(AppMessage::decode(&plain).unwrap(), message);
        assert!(receiver.decrypt(&sealed).is_err(), "a replay is refused");
        let mut stranger = FrameReceiver::new(&[8u8; 32], 3).unwrap();
        assert!(stranger.decrypt(&sealed).is_err());
    }

    #[test]
    fn the_longest_chat_fits_one_sealed_message() {
        // Four bytes a character is the worst case for UTF-8.
        let message = AppMessage::Chat("🎉".repeat(MAX_CHAT_CHARS));
        assert!(message.encode().unwrap().len() <= MAX_PLAIN_LEN);
    }
}
