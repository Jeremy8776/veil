//! App messages between members: each member's display name, raised hand, and chat.
//!
//! Purpose: names, hands and chat ride the media links next to video and sound, sealed with the
//! sender's frame key for the current epoch ([`AppMessage::seal`]), so relays and forwarders
//! see only ciphertext and its size. The frame key is shared by every member, so it cannot say
//! which member sent a message: any member can seal under any sender index. Each message is
//! therefore also signed with the sender's device key. The receiver opens the packet with
//! `KeyRing::open_app`, which looks up the device key of the leaf the frame names in that
//! epoch's roster and calls [`AppMessage::open`], which refuses the message unless that device
//! signed it. Who sent a message is that signed device, never the link it came on.
//!
//! Wire format (the plaintext inside the sealed frame):
//! - 8 bytes `veil-app`, then a version byte (3). Video and sound never start like this, so
//!   a forwarder that moves a sealed frame from one channel to another gets it refused here.
//! - a 64-byte Ed25519 signature by the sender's device key over a domain tag, the epoch's
//!   binding (`veil_security::frame::FrameSender::binding`), the frame id (key id and counter,
//!   see `veil_security::frame::frame_id`) and the body.
//! - the body: a postcard-encoded [`AppMessage`], with nothing after it. New kinds are added
//!   only at the end of the enum, so older kinds keep their encoding. An older Veil refuses a
//!   kind it does not know as `Malformed` and carries on.
//!
//! Invariants:
//! - The signature covers the frame id, which names the epoch, the sender index and a counter
//!   used once. So a member cannot sign as another device, and cannot lift a signed message
//!   into another frame (a later counter, another sender index or another epoch).
//! - The binding differs for every MLS group and epoch. Epoch numbers, leaf indexes and
//!   counters repeat from call to call and a device keeps its key, so without it a member
//!   could replay a line the victim signed in an earlier call (GHSA-qqx7-3x2m-v277).
//! - Every message is checked on the way in and out: names 1 to [`MAX_NAME_CHARS`]
//!   characters, chat 1 to [`MAX_CHAT_CHARS`], a reaction 1 to [`MAX_REACTION_CHARS`] with no
//!   spaces, no control characters (chat may hold line breaks and tabs) and no bidirectional
//!   overrides, which could make a name read as another.
//! - Chat is sent as [`AppMessage::Post`], whose id the sender picks at random, so others can
//!   react to it. A message is named by its id alone: the receiver keeps the first message
//!   with an id, so a member cannot take over another's message by reusing its id. Plain
//!   [`AppMessage::Chat`] from 0.1.11 and earlier is still read, but takes no reactions.
//! - New variants go at the end only: postcard numbers them in order, so an older app reads
//!   names and plain chat from a newer one and drops what it does not know as malformed.
//! - A sealed message is at most `veil_net::control::MAX_APP_LEN` bytes; larger plaintext is
//!   refused before decoding.
//! - Replays are refused by the frame key's signed-frame window, which moves only once the
//!   signature checked out, so a forged frame cannot stop a member's names and chat.

use std::fmt;

use serde::{Deserialize, Serialize};
use veil_net::control::MAX_APP_LEN;
use veil_security::frame::{BINDING_LEN, FrameSender, HEADER_LEN, ID_LEN, TAG_LEN, frame_id};
use veil_security::{Identity, PublicId};

/// The longest display name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// The longest chat message, in characters.
pub const MAX_CHAT_CHARS: usize = 2_000;
/// The longest reaction, in characters: room for one emoji with a skin tone or a joiner.
pub const MAX_REACTION_CHARS: usize = 8;

const MAGIC: &[u8; 8] = b"veil-app";
const VERSION: u8 = 3;
const SIGNATURE_LEN: usize = 64;
const SIGN_DOMAIN: &[u8] = b"veil-app-message-v3";
const PREFIX_LEN: usize = MAGIC.len() + 1 + SIGNATURE_LEN;
/// The largest plaintext that still seals within `MAX_APP_LEN`.
const MAX_PLAIN_LEN: usize = MAX_APP_LEN - HEADER_LEN - TAG_LEN;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum AppMessage {
    /// The sender's display name. Sent on joining and after each epoch change.
    Name(String),
    /// One chat message to everyone in the call, from 0.1.11 and earlier. Read, never sent.
    Chat(String),
    /// Whether the sender's hand is raised. Sent when it changes, on joining and after each
    /// epoch change, so someone who joins later sees a hand that is already up.
    Hand(bool),
    /// One chat message to everyone in the call, with an id others can react to.
    Post { id: u64, text: String },
    /// Adds (`on`) or takes back the sender's reaction to the message with `id`.
    React { id: u64, emoji: String, on: bool },
}

/// A random id for a new chat message.
pub fn new_message_id() -> u64 {
    let mut bytes = [0u8; 8];
    // shortcut: a failed system RNG falls back to the clock, which only risks two of your own
    // messages sharing an id (the second then takes no reactions). Upgrade if ids gain weight.
    if getrandom::fill(&mut bytes).is_err() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        return now.map_or(0, |d| d.as_secs() ^ (u64::from(d.subsec_nanos()) << 32));
    }
    u64::from_le_bytes(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    Empty,
    TooLong,
    /// A control character or a bidirectional override.
    BadCharacter,
    /// Not an app message, an unknown version, or trailing bytes.
    Malformed,
    /// Not signed by the device the frame names, or signed for another frame.
    BadSignature,
    /// The frame key refused to seal (its counter ran out).
    Seal,
    /// The frame key did not open it: another epoch, a replay, the wrong link, or forged.
    Unopened,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty message",
            Self::TooLong => "message too long",
            Self::BadCharacter => "message holds a character that is not allowed",
            Self::Malformed => "not a valid app message",
            Self::BadSignature => "message not signed by its sender",
            Self::Seal => "cannot seal the message",
            Self::Unopened => "the frame key did not open the message",
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

    /// A chat message from what the person typed, trimmed, with the id it shows under.
    pub fn chat(id: u64, text: &str) -> Result<Self, AppError> {
        let message = Self::Post {
            id,
            text: text.trim().to_string(),
        };
        message.check()?;
        Ok(message)
    }

    /// Adds (`on`) or takes back a reaction to the message with `id`.
    pub fn react(id: u64, emoji: &str, on: bool) -> Result<Self, AppError> {
        let message = Self::React {
            id,
            emoji: emoji.to_string(),
            on,
        };
        message.check()?;
        Ok(message)
    }

    /// Checks the length and characters of the text.
    pub fn check(&self) -> Result<(), AppError> {
        let (text, max, allow_breaks) = match self {
            Self::Name(text) => (text, MAX_NAME_CHARS, false),
            Self::Hand(_) => return Ok(()),
            Self::Chat(text) | Self::Post { text, .. } => (text, MAX_CHAT_CHARS, true),
            Self::React { emoji, .. } => (emoji, MAX_REACTION_CHARS, false),
        };
        if text.trim().is_empty() {
            return Err(AppError::Empty);
        }
        if text.chars().count() > max {
            return Err(AppError::TooLong);
        }
        if matches!(self, Self::React { .. }) && text.chars().any(char::is_whitespace) {
            return Err(AppError::BadCharacter);
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

    /// Signs with `identity` (this device's key) for the frame `sender` seals next, then
    /// seals, ready for `MediaSession::send_app`.
    pub fn seal(&self, sender: &mut FrameSender, identity: &Identity) -> Result<Vec<u8>, AppError> {
        self.check()?;
        let body = postcard::to_allocvec(self).map_err(|_| AppError::Malformed)?;
        let plain = signed_plain(identity, sender.binding(), &sender.next_frame_id(), &body);
        if plain.len() > MAX_PLAIN_LEN {
            return Err(AppError::TooLong);
        }
        sender.encrypt(&plain).map_err(|_| AppError::Seal)
    }

    /// Reads an app message the frame key opened. `sealed` is the packet as it arrived,
    /// `plain` what the frame key opened, `device` the roster's device key for the leaf the
    /// frame key named, and `binding` that epoch's `FrameReceiver::binding`. Refuses a message
    /// `device` did not sign for this very frame in this very group and epoch.
    pub fn open(
        sealed: &[u8],
        plain: &[u8],
        device: &PublicId,
        binding: &[u8; BINDING_LEN],
    ) -> Result<Self, AppError> {
        if plain.len() > MAX_PLAIN_LEN {
            return Err(AppError::TooLong);
        }
        let rest = plain
            .strip_prefix(MAGIC.as_slice())
            .and_then(|rest| rest.strip_prefix(&[VERSION]))
            .ok_or(AppError::Malformed)?;
        let (signature, body) = rest
            .split_first_chunk::<SIGNATURE_LEN>()
            .ok_or(AppError::Malformed)?;
        let id = frame_id(sealed).ok_or(AppError::Malformed)?;
        device
            .verify(&signed_message(binding, &id, body), signature)
            .map_err(|_| AppError::BadSignature)?;
        let (message, trailing): (Self, _) =
            postcard::take_from_bytes(body).map_err(|_| AppError::Malformed)?;
        if !trailing.is_empty() {
            return Err(AppError::Malformed);
        }
        message.check()?;
        Ok(message)
    }
}

/// What the sender signs: a domain tag, the epoch's binding, the frame id and the body.
fn signed_message(binding: &[u8; BINDING_LEN], id: &[u8; ID_LEN], body: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(SIGN_DOMAIN.len() + BINDING_LEN + ID_LEN + body.len());
    message.extend_from_slice(SIGN_DOMAIN);
    message.extend_from_slice(binding);
    message.extend_from_slice(id);
    message.extend_from_slice(body);
    message
}

/// The plaintext to seal: the prefix, the signature, then the body.
fn signed_plain(
    identity: &Identity,
    binding: &[u8; BINDING_LEN],
    id: &[u8; ID_LEN],
    body: &[u8],
) -> Vec<u8> {
    let mut plain = Vec::with_capacity(PREFIX_LEN + body.len());
    plain.extend_from_slice(MAGIC);
    plain.push(VERSION);
    plain.extend_from_slice(&identity.sign(&signed_message(binding, id, body)));
    plain.extend_from_slice(body);
    plain
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

    const SECRET: [u8; 32] = [7; 32];
    const EPOCH: u64 = 3;

    /// Seals `body` signed by `signer` as sender `leaf`, skipping every check `seal` makes.
    fn forge(signer: &Identity, leaf: u32, body: &[u8]) -> Vec<u8> {
        let mut sender = FrameSender::new(&SECRET, EPOCH, leaf).unwrap();
        let plain = signed_plain(signer, sender.binding(), &sender.next_frame_id(), body);
        sender.encrypt(&plain).unwrap()
    }

    /// Opens `sealed` as a receiver whose roster says `device` holds the leaf the frame names.
    fn receive(sealed: &[u8], device: &PublicId) -> Result<AppMessage, AppError> {
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(sealed).unwrap();
        AppMessage::open(sealed, &plain, device, receiver.binding())
    }

    #[test]
    fn names_hands_and_chat_round_trip_and_name_the_sender() {
        let jo = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, 1).unwrap();
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        for message in [
            AppMessage::name("  Jo Bloggs ").unwrap(),
            AppMessage::chat(1, "Hello\nsecond line\twith a tab, and émoji 🎉").unwrap(),
            AppMessage::Hand(true),
            AppMessage::Hand(false),
        ] {
            let sealed = message.seal(&mut sender, &jo).unwrap();
            let (leaf, plain) = receiver.decrypt(&sealed).unwrap();
            assert_eq!(leaf, 1);
            assert!(plain.starts_with(b"veil-app\x03"));
            assert_eq!(AppMessage::open(&sealed, &plain, &jo.public(), receiver.binding()).unwrap(), message);
        }
        assert_eq!(AppMessage::name(" Jo ").unwrap(), AppMessage::Name("Jo".into()));
    }

    /// Names and chat keep the encoding older versions read: the hand only adds a kind.
    #[test]
    fn older_kinds_keep_their_encoding() {
        assert_eq!(postcard::to_allocvec(&AppMessage::Name("Jo".into())).unwrap(), [0, 2, b'J', b'o']);
        assert_eq!(postcard::to_allocvec(&AppMessage::Chat("hi".into())).unwrap(), [1, 2, b'h', b'i']);
        assert_eq!(postcard::to_allocvec(&AppMessage::Hand(true)).unwrap(), [2, 1]);
    }

    #[test]
    fn a_member_cannot_speak_as_another() {
        let (alice, bob) = (Identity::generate(), Identity::generate());
        // Alice holds the shared epoch secret, so she can seal under Bob's leaf (2), but
        // cannot sign with Bob's device key.
        let as_bob = AppMessage::chat(1, "Bob here, please send the file")
            .unwrap()
            .seal(&mut FrameSender::new(&SECRET, EPOCH, 2).unwrap(), &alice)
            .unwrap();
        assert_eq!(receive(&as_bob, &bob.public()), Err(AppError::BadSignature));
    }

    #[test]
    fn a_signed_message_cannot_move_to_another_frame() {
        let (alice, bob) = (Identity::generate(), Identity::generate());
        let mut bob_sender = FrameSender::new(&SECRET, EPOCH, 2).unwrap();
        let original = AppMessage::chat(1, "yes").unwrap().seal(&mut bob_sender, &bob).unwrap();
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(&original).unwrap();

        // Alice re-seals Bob's signed plaintext under Bob's leaf with a later counter.
        let mut replayer = FrameSender::new(&SECRET, EPOCH, 2).unwrap();
        replayer.encrypt(b"burn counter 0").unwrap();
        let moved = replayer.encrypt(&plain).unwrap();
        assert_eq!(receive(&moved, &bob.public()), Err(AppError::BadSignature));

        // Or under her own leaf, claiming the words as hers.
        let mut hers = FrameSender::new(&SECRET, EPOCH, 1).unwrap();
        let moved = hers.encrypt(&plain).unwrap();
        assert_eq!(receive(&moved, &alice.public()), Err(AppError::BadSignature));
    }

    /// GHSA-qqx7-3x2m-v277: a line the victim signed in one call does not open in another
    /// call at the same epoch number, leaf and counter.
    #[test]
    fn a_signed_message_cannot_move_to_another_call() {
        let victim = Identity::generate();
        let mut earlier = FrameSender::new(&[1; 32], EPOCH, 2).unwrap();
        let original = AppMessage::chat(1, "approved")
            .unwrap()
            .seal(&mut earlier, &victim)
            .unwrap();
        let (_, plain) = FrameReceiver::new(&[1; 32], EPOCH)
            .unwrap()
            .decrypt(&original)
            .unwrap();

        // In the later call a member re-seals the plaintext under the victim's leaf and counter.
        let mut later = FrameSender::new(&SECRET, EPOCH, 2).unwrap();
        let replayed = later.encrypt(&plain).unwrap();
        assert_eq!(
            frame_id(&replayed),
            frame_id(&original),
            "same epoch, leaf and counter"
        );
        assert_eq!(
            receive(&replayed, &victim.public()),
            Err(AppError::BadSignature)
        );

        // The victim's own line in the later call still opens.
        let mut own = FrameSender::new(&SECRET, EPOCH, 2).unwrap();
        let genuine = AppMessage::chat(1, "hello")
            .unwrap()
            .seal(&mut own, &victim)
            .unwrap();
        assert_eq!(
            receive(&genuine, &victim.public()).unwrap(),
            AppMessage::Post {
                id: 1,
                text: "hello".into()
            }
        );
    }

    #[test]
    fn reactions_and_old_chat_round_trip() {
        let jo = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, 1).unwrap();
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        for message in [
            AppMessage::react(42, "👍", true).unwrap(),
            AppMessage::react(42, "👍🏽", false).unwrap(),
            // What 0.1.11 sends is still read.
            AppMessage::Chat("from an older app".into()),
        ] {
            let sealed = message.seal(&mut sender, &jo).unwrap();
            let (_, plain) = receiver.decrypt(&sealed).unwrap();
            assert_eq!(AppMessage::open(&sealed, &plain, &jo.public(), receiver.binding()).unwrap(), message);
        }
    }

    #[test]
    fn bad_reactions_are_refused() {
        assert_eq!(AppMessage::react(1, "", true), Err(AppError::Empty));
        assert_eq!(AppMessage::react(1, "👍 👍", true), Err(AppError::BadCharacter));
        assert_eq!(AppMessage::react(1, "\u{202E}x", true), Err(AppError::BadCharacter));
        assert_eq!(AppMessage::react(1, &"x".repeat(9), true), Err(AppError::TooLong));
        let jo = Identity::generate();
        let forged = AppMessage::React {
            id: 1,
            emoji: "a\nb".into(),
            on: true,
        };
        let body = postcard::to_allocvec(&forged).unwrap();
        assert_eq!(receive(&forge(&jo, 1, &body), &jo.public()), Err(AppError::BadCharacter));
    }

    #[test]
    fn message_ids_differ() {
        assert_ne!(new_message_id(), new_message_id());
    }

    #[test]
    fn bad_text_is_refused_both_ways() {
        assert_eq!(AppMessage::name("   "), Err(AppError::Empty));
        assert_eq!(AppMessage::chat(1, ""), Err(AppError::Empty));
        assert_eq!(AppMessage::name(&"a".repeat(65)), Err(AppError::TooLong));
        assert!(
            AppMessage::name(&"é".repeat(64)).is_ok(),
            "counts characters, not bytes"
        );
        assert_eq!(AppMessage::chat(1, &"a".repeat(2_001)), Err(AppError::TooLong));
        assert_eq!(AppMessage::name("Jo\nBloggs"), Err(AppError::BadCharacter));
        assert_eq!(
            AppMessage::name("Jo\u{202E}sggolB"),
            Err(AppError::BadCharacter)
        );
        assert_eq!(AppMessage::chat(1, "bell\u{7}"), Err(AppError::BadCharacter));

        // A sender that skips the checks is still refused on the way in.
        let jo = Identity::generate();
        let body = postcard::to_allocvec(&AppMessage::Name("a\u{0}b".into())).unwrap();
        assert_eq!(receive(&forge(&jo, 1, &body), &jo.public()), Err(AppError::BadCharacter));
        let body = postcard::to_allocvec(&AppMessage::Chat("a".repeat(3_000))).unwrap();
        assert_eq!(receive(&forge(&jo, 1, &body), &jo.public()), Err(AppError::TooLong));
    }

    #[test]
    fn other_bytes_are_not_app_messages() {
        let jo = Identity::generate();
        let mut body = postcard::to_allocvec(&AppMessage::Chat("hi".into())).unwrap();
        body.push(0);
        assert_eq!(receive(&forge(&jo, 1, &body), &jo.public()), Err(AppError::Malformed));

        let mut sender = FrameSender::new(&SECRET, EPOCH, 1).unwrap();
        let sealed = AppMessage::chat(1, "hi").unwrap().seal(&mut sender, &jo).unwrap();
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(&sealed).unwrap();
        let mut version = plain.clone();
        version[8] = 1;
        let binding = receiver.binding();
        assert_eq!(AppMessage::open(&sealed, &version, &jo.public(), binding), Err(AppError::Malformed));
        assert_eq!(
            AppMessage::open(&sealed, &plain[..PREFIX_LEN - 1], &jo.public(), binding),
            Err(AppError::Malformed)
        );
        assert_eq!(AppMessage::open(&sealed[..4], &plain, &jo.public(), binding), Err(AppError::Malformed));
        assert_eq!(
            AppMessage::open(&sealed, &vec![0; MAX_PLAIN_LEN + 1], &jo.public(), binding),
            Err(AppError::TooLong)
        );
    }

    #[test]
    fn sealed_messages_open_only_with_the_epoch_key_and_once() {
        let jo = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, 1).unwrap();
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let message = AppMessage::chat(1, &"x".repeat(MAX_CHAT_CHARS)).unwrap();
        let sealed = message.seal(&mut sender, &jo).unwrap();
        assert!(sealed.len() <= MAX_APP_LEN);
        assert!(
            !sealed.windows(8).any(|w| w == b"veil-app"),
            "nothing readable on the wire"
        );
        let (leaf, plain) = receiver.decrypt(&sealed).unwrap();
        assert_eq!(leaf, 1);
        assert_eq!(AppMessage::open(&sealed, &plain, &jo.public(), receiver.binding()).unwrap(), message);
        assert!(receiver.decrypt(&sealed).is_err(), "a replay is refused");
        let mut stranger = FrameReceiver::new(&[8u8; 32], EPOCH).unwrap();
        assert!(stranger.decrypt(&sealed).is_err());
    }

    #[test]
    fn the_longest_chat_fits_one_sealed_message() {
        // Four bytes a character is the worst case for UTF-8.
        // The largest id takes the most bytes.
        let message = AppMessage::Post {
            id: u64::MAX,
            text: "🎉".repeat(MAX_CHAT_CHARS),
        };
        let mut sender = FrameSender::new(&SECRET, EPOCH, 1).unwrap();
        let sealed = message.seal(&mut sender, &Identity::generate()).unwrap();
        assert!(sealed.len() <= MAX_APP_LEN);
    }
}
