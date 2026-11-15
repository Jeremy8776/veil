//! Per-frame media encryption, in the style of SFrame (RFC 9605).
//!
//! Every encoded video or audio frame is sealed with ChaCha20-Poly1305 before it leaves the
//! sender. Relays and forwarders see only the 16-byte header and the ciphertext, so they can
//! route and drop frames but never read or forge them.
//!
//! Keys: the MLS group gives every member the same 32-byte epoch secret. Each sender's key and
//! nonce salt are derived from it with HKDF-SHA256 and the sender's key id, where
//! `key_id = epoch << 16 | sender_index`. Distinct senders therefore never share a key, which
//! is what makes a per-sender counter a safe nonce.
//!
//! Header: key id (u64, big endian), frame counter (u64, big endian), then the two-byte
//! [`Layer`] marker a forwarder reads to drop SVC layers. The whole header is the AEAD's
//! associated data, so changing any of it breaks authentication.
//!
//! Replay: each receiver keeps a 64-frame sliding window per sender. A counter seen before, or
//! more than 64 behind the newest, is refused. State for a sender is stored only after one of
//! its frames authenticates, so forged packets cannot grow memory.

use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::SecurityError;
use crate::layer::{LAYER_LEN, Layer};

const ID_LEN: usize = 16;
pub const HEADER_LEN: usize = ID_LEN + LAYER_LEN;
pub const TAG_LEN: usize = 16;
/// Largest frame accepted. A 4K AV1 keyframe is well under this.
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;
pub const MAX_SENDER_INDEX: u32 = 0xFFFF;
/// Epochs must fit in the top 48 bits of the key id.
pub const MAX_EPOCH: u64 = (1 << 48) - 1;
const WINDOW: u64 = 64;
const SALT: &[u8] = b"veil-frame-v1";

/// Builds the key id for a sender in an epoch.
pub fn key_id(epoch: u64, sender_index: u32) -> Result<u64, SecurityError> {
    if sender_index > MAX_SENDER_INDEX {
        return Err(SecurityError::Malformed("sender index"));
    }
    if epoch > MAX_EPOCH {
        return Err(SecurityError::Malformed("epoch"));
    }
    Ok((epoch << 16) | u64::from(sender_index))
}

struct KeyMaterial {
    cipher: ChaCha20Poly1305,
    salt: [u8; 12],
}

fn derive(epoch_secret: &[u8; 32], kid: u64) -> Result<KeyMaterial, SecurityError> {
    let hk = Hkdf::<Sha256>::new(Some(SALT), epoch_secret);
    let kid_bytes = kid.to_be_bytes();
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand_multi_info(&[b"key", &kid_bytes], key.as_mut())
        .map_err(|_| SecurityError::BadKey)?;
    let mut salt = [0u8; 12];
    hk.expand_multi_info(&[b"salt", &kid_bytes], &mut salt)
        .map_err(|_| SecurityError::BadKey)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| SecurityError::BadKey)?;
    Ok(KeyMaterial { cipher, salt })
}

fn nonce(salt: &[u8; 12], counter: u64) -> Nonce {
    let mut n = *salt;
    for (slot, byte) in n[4..].iter_mut().zip(counter.to_be_bytes()) {
        *slot ^= byte;
    }
    Nonce::from(n)
}

fn header(kid: u64, counter: u64, layer: Layer) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(&kid.to_be_bytes());
    h[8..ID_LEN].copy_from_slice(&counter.to_be_bytes());
    h[ID_LEN..].copy_from_slice(&layer.encode());
    h
}

/// Reads a sealed frame's layer marker without opening it. Forwarders use this; it is not
/// authenticated until the frame is decrypted.
pub fn frame_layer(packet: &[u8]) -> Result<Layer, SecurityError> {
    let bytes = packet
        .get(ID_LEN..HEADER_LEN)
        .and_then(|b| <[u8; LAYER_LEN]>::try_from(b).ok())
        .ok_or(SecurityError::Malformed("short packet"))?;
    Layer::decode(bytes)
}

/// Seals one sender's frames for one epoch.
pub struct FrameSender {
    kid: u64,
    keys: KeyMaterial,
    next: u64,
}

impl FrameSender {
    pub fn new(epoch_secret: &[u8; 32], epoch: u64, sender_index: u32) -> Result<Self, SecurityError> {
        let kid = key_id(epoch, sender_index)?;
        Ok(Self {
            kid,
            keys: derive(epoch_secret, kid)?,
            next: 0,
        })
    }

    /// Seals audio or single-layer video as [`Layer::BASE`].
    pub fn encrypt(&mut self, frame: &[u8]) -> Result<Vec<u8>, SecurityError> {
        self.encrypt_layer(Layer::BASE, frame)
    }

    /// Returns `header || ciphertext || tag`, with `layer` in the plaintext header.
    pub fn encrypt_layer(&mut self, layer: Layer, frame: &[u8]) -> Result<Vec<u8>, SecurityError> {
        if frame.len() > MAX_FRAME_LEN {
            return Err(SecurityError::TooLarge);
        }
        if self.next == u64::MAX {
            return Err(SecurityError::CounterExhausted);
        }
        let counter = self.next;
        let head = header(self.kid, counter, layer);
        let sealed = self
            .keys
            .cipher
            .encrypt(
                &nonce(&self.keys.salt, counter),
                Payload {
                    msg: frame,
                    aad: &head,
                },
            )
            .map_err(|_| SecurityError::Encrypt)?;
        self.next += 1;
        let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
        out.extend_from_slice(&head);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    #[cfg(test)]
    fn set_counter(&mut self, value: u64) {
        self.next = value;
    }
}

#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    /// Bit `i` set means counter `highest - i` was accepted.
    seen: u64,
}

impl ReplayWindow {
    fn check(&self, counter: u64) -> Result<(), SecurityError> {
        let Some(highest) = self.highest else {
            return Ok(());
        };
        if counter > highest {
            return Ok(());
        }
        let age = highest - counter;
        if age >= WINDOW || self.seen & (1 << age) != 0 {
            return Err(SecurityError::Replay);
        }
        Ok(())
    }

    fn accept(&mut self, counter: u64) {
        match self.highest {
            None => {
                self.highest = Some(counter);
                self.seen = 1;
            }
            Some(highest) if counter > highest => {
                let shift = counter - highest;
                self.seen = if shift >= WINDOW { 0 } else { self.seen << shift };
                self.seen |= 1;
                self.highest = Some(counter);
            }
            Some(highest) => self.seen |= 1 << (highest - counter),
        }
    }
}

struct SenderState {
    keys: KeyMaterial,
    window: ReplayWindow,
}

/// Opens frames from every sender in one epoch.
pub struct FrameReceiver {
    epoch_secret: Zeroizing<[u8; 32]>,
    epoch: u64,
    senders: BTreeMap<u64, SenderState>,
}

impl FrameReceiver {
    pub fn new(epoch_secret: &[u8; 32], epoch: u64) -> Result<Self, SecurityError> {
        if epoch > MAX_EPOCH {
            return Err(SecurityError::Malformed("epoch"));
        }
        Ok(Self {
            epoch_secret: Zeroizing::new(*epoch_secret),
            epoch,
            senders: BTreeMap::new(),
        })
    }

    /// Returns the sender index and the plaintext frame.
    pub fn decrypt(&mut self, packet: &[u8]) -> Result<(u32, Vec<u8>), SecurityError> {
        if packet.len() < HEADER_LEN + TAG_LEN {
            return Err(SecurityError::Malformed("short packet"));
        }
        if packet.len() > HEADER_LEN + MAX_FRAME_LEN + TAG_LEN {
            return Err(SecurityError::TooLarge);
        }
        frame_layer(packet)?;
        let (head, sealed) = packet.split_at(HEADER_LEN);
        let mut kid_bytes = [0u8; 8];
        let mut counter_bytes = [0u8; 8];
        kid_bytes.copy_from_slice(&head[..8]);
        counter_bytes.copy_from_slice(&head[8..ID_LEN]);
        let kid = u64::from_be_bytes(kid_bytes);
        let counter = u64::from_be_bytes(counter_bytes);
        if kid >> 16 != self.epoch {
            return Err(SecurityError::UnknownKey);
        }
        let sender = u32::from(u16::try_from(kid & 0xFFFF).map_err(|_| SecurityError::UnknownKey)?);

        if let Some(state) = self.senders.get_mut(&kid) {
            return open(state, head, counter, sealed).map(|plain| (sender, plain));
        }
        let mut state = SenderState {
            keys: derive(&self.epoch_secret, kid)?,
            window: ReplayWindow::default(),
        };
        let plain = open(&mut state, head, counter, sealed)?;
        self.senders.insert(kid, state);
        Ok((sender, plain))
    }
}

fn open(
    state: &mut SenderState,
    head: &[u8],
    counter: u64,
    sealed: &[u8],
) -> Result<Vec<u8>, SecurityError> {
    state.window.check(counter)?;
    let plain = state
        .keys
        .cipher
        .decrypt(
            &nonce(&state.keys.salt, counter),
            Payload {
                msg: sealed,
                aad: head,
            },
        )
        .map_err(|_| SecurityError::Decrypt)?;
    state.window.accept(counter);
    Ok(plain)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [42; 32];

    #[test]
    fn round_trip_names_the_sender() {
        let mut tx = FrameSender::new(&SECRET, 3, 7).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 3).unwrap();
        let packet = tx.encrypt(b"frame one").unwrap();
        assert_eq!(rx.decrypt(&packet).unwrap(), (7, b"frame one".to_vec()));
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let mut tx = FrameSender::new(&SECRET, 0, 0).unwrap();
        let packet = tx.encrypt(b"secret secret secret").unwrap();
        assert!(!packet.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn tampered_body_is_refused() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let mut packet = tx.encrypt(b"frame").unwrap();
        packet[HEADER_LEN] ^= 1;
        assert_eq!(rx.decrypt(&packet), Err(SecurityError::Decrypt));
    }

    #[test]
    fn forwarder_cannot_relabel_the_sender() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let mut packet = tx.encrypt(b"frame").unwrap();
        packet[7] = 2; // claim sender 2
        assert_eq!(rx.decrypt(&packet), Err(SecurityError::Decrypt));
    }

    #[test]
    fn layer_is_readable_but_not_changeable() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let layer = Layer::new(2, 1, true).unwrap();
        let packet = tx.encrypt_layer(layer, b"uhd frame").unwrap();
        assert_eq!(frame_layer(&packet).unwrap(), layer);
        let mut relabelled = packet.clone();
        relabelled[ID_LEN + 1] = 0x00; // claim the base layer
        assert_eq!(rx.decrypt(&relabelled), Err(SecurityError::Decrypt));
        assert_eq!(rx.decrypt(&packet).unwrap().1, b"uhd frame");
    }

    #[test]
    fn unknown_layer_bits_are_refused_before_decrypting() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let mut packet = tx.encrypt(b"frame").unwrap();
        packet[ID_LEN] = 0x80;
        assert!(matches!(rx.decrypt(&packet), Err(SecurityError::Malformed(_))));
        assert_eq!(frame_layer(&tx.encrypt(b"audio").unwrap()).unwrap(), Layer::BASE);
    }

    #[test]
    fn replay_is_refused() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let packet = tx.encrypt(b"frame").unwrap();
        assert!(rx.decrypt(&packet).is_ok());
        assert_eq!(rx.decrypt(&packet), Err(SecurityError::Replay));
    }

    #[test]
    fn reordering_inside_window_is_accepted() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let packets: Vec<_> = (0..5).map(|_| tx.encrypt(b"f").unwrap()).collect();
        for i in [4, 0, 2, 1, 3] {
            assert!(rx.decrypt(&packets[i]).is_ok(), "packet {i}");
        }
    }

    #[test]
    fn frame_older_than_window_is_refused() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        let old = tx.encrypt(b"old").unwrap();
        for _ in 0..WINDOW {
            let p = tx.encrypt(b"new").unwrap();
            assert!(rx.decrypt(&p).is_ok());
        }
        assert_eq!(rx.decrypt(&old), Err(SecurityError::Replay));
    }

    #[test]
    fn other_epoch_is_unknown() {
        let mut tx = FrameSender::new(&SECRET, 4, 1).unwrap();
        let mut rx = FrameReceiver::new(&SECRET, 5).unwrap();
        let packet = tx.encrypt(b"frame").unwrap();
        assert_eq!(rx.decrypt(&packet), Err(SecurityError::UnknownKey));
    }

    #[test]
    fn removed_member_with_old_secret_cannot_read() {
        let mut tx = FrameSender::new(&[1; 32], 9, 1).unwrap();
        let mut rx = FrameReceiver::new(&[2; 32], 9).unwrap();
        let packet = tx.encrypt(b"frame").unwrap();
        assert_eq!(rx.decrypt(&packet), Err(SecurityError::Decrypt));
    }

    #[test]
    fn forged_packets_do_not_grow_receiver_state() {
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        for sender in 0..100u64 {
            let mut forged = vec![0u8; HEADER_LEN + TAG_LEN + 4];
            forged[..8].copy_from_slice(&sender.to_be_bytes());
            assert!(rx.decrypt(&forged).is_err());
        }
        assert!(rx.senders.is_empty());
    }

    #[test]
    fn counter_never_wraps() {
        let mut tx = FrameSender::new(&SECRET, 0, 1).unwrap();
        tx.set_counter(u64::MAX);
        assert_eq!(tx.encrypt(b"f"), Err(SecurityError::CounterExhausted));
    }

    #[test]
    fn bounds_are_enforced() {
        assert!(key_id(0, MAX_SENDER_INDEX + 1).is_err());
        assert!(key_id(MAX_EPOCH + 1, 0).is_err());
        let mut rx = FrameReceiver::new(&SECRET, 0).unwrap();
        assert!(rx.decrypt(&[0u8; 8]).is_err());
    }
}
