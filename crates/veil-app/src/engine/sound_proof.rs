//! Proof of whose share a sound belongs to: every packet of a share's computer sound is signed
//! by the sender's device, as screen pictures are (`screen_proof.rs`, ADR 0007).
//!
//! Purpose: all members hold the epoch secret, so the frame key cannot say which member sealed
//! a packet. Without a signature, a member could play sound into someone else's share through a
//! forwarder. The sender signs each packet with its device key, and the receiver checks it
//! against the device the roster gives for the packet's leaf.
//!
//! Wire format of a share sound packet's plaintext: the 13-byte audio header, a 64-byte
//! Ed25519 signature, then the Opus packet. The signature covers a domain tag, the epoch's
//! binding, the frame id, the header and the Opus packet.
//!
//! Invariants:
//! - A packet marked as share sound reaches the decoder only with a valid signature; there is
//!   no unsigned form.
//! - The frame id and binding tie a signature to one packet of one call and epoch.
//! - The microphone is not signed (`docs/TRANSPARENCY.md`); only share sound is.

use veil_media::AudioHeader;
use veil_media::audio::AUDIO_HEADER_LEN;
use veil_security::frame::{BINDING_LEN, ID_LEN, frame_id};
use veil_security::{Identity, PublicId};

const DOMAIN: &[u8] = b"veil-share-sound-v1";
const SIGNATURE_LEN: usize = 64;

/// True if `payload` starts with a valid audio header marked as share sound.
pub fn is_share_sound(payload: &[u8]) -> bool {
    AudioHeader::read(payload).is_ok_and(|(header, _)| header.share)
}

fn signed_message(
    binding: &[u8; BINDING_LEN],
    id: &[u8; ID_LEN],
    head: &[u8],
    packet: &[u8],
) -> Vec<u8> {
    [DOMAIN, binding.as_slice(), id.as_slice(), head, packet].concat()
}

/// `payload` with the signature for the frame `id` added, ready to seal.
pub fn sign(
    identity: &Identity,
    binding: &[u8; BINDING_LEN],
    id: &[u8; ID_LEN],
    payload: &[u8],
) -> Option<Vec<u8>> {
    let (head, packet) = payload.split_at_checked(AUDIO_HEADER_LEN)?;
    let signature = identity.sign(&signed_message(binding, id, head, packet));
    Some([head, signature.as_slice(), packet].concat())
}

/// The opened packet without its signature, if `device` signed it for this very frame.
/// `packet` is the sealed frame as it arrived.
pub fn check(
    device: &PublicId,
    binding: &[u8; BINDING_LEN],
    packet: &[u8],
    plain: Vec<u8>,
) -> Option<Vec<u8>> {
    let id = frame_id(packet)?;
    let (head, rest) = plain.split_at_checked(AUDIO_HEADER_LEN)?;
    let (signature, opus) = rest.split_first_chunk::<SIGNATURE_LEN>()?;
    device
        .verify(&signed_message(binding, &id, head, opus), signature)
        .ok()?;
    let mut payload = plain;
    payload.drain(AUDIO_HEADER_LEN..AUDIO_HEADER_LEN + SIGNATURE_LEN);
    Some(payload)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_security::frame::{FrameReceiver, FrameSender};

    use super::*;

    const SECRET: [u8; 32] = [7; 32];
    const EPOCH: u64 = 3;
    const LEAF: u32 = 2;

    fn sound() -> Vec<u8> {
        let header = AudioHeader {
            sequence: 1,
            timestamp_us: 2,
            share: true,
        };
        header.write(b"opus").unwrap()
    }

    fn seal(sender: &mut FrameSender, signer: &Identity) -> Vec<u8> {
        let (binding, id) = (*sender.binding(), sender.next_frame_id());
        let signed = sign(signer, &binding, &id, &sound()).unwrap();
        sender.encrypt(&signed).unwrap()
    }

    fn receive(sealed: &[u8], device: &PublicId) -> Option<Vec<u8>> {
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(sealed).unwrap();
        check(device, receiver.binding(), sealed, plain)
    }

    #[test]
    fn signed_share_sound_arrives_as_it_was_encoded() {
        let alice = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let sealed = seal(&mut sender, &alice);
        assert_eq!(receive(&sealed, &alice.public()), Some(sound()));
        assert!(is_share_sound(&sound()));
    }

    #[test]
    fn a_member_cannot_play_sound_into_anothers_share() {
        let (alice, mallory) = (Identity::generate(), Identity::generate());
        let mut as_alice = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let forged = seal(&mut as_alice, &mallory);
        assert_eq!(receive(&forged, &alice.public()), None);
        let unsigned = as_alice.encrypt(&sound()).unwrap();
        assert_eq!(receive(&unsigned, &alice.public()), None, "no unsigned form");
    }

    #[test]
    fn signed_sound_cannot_move_to_another_frame() {
        let alice = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let original = seal(&mut sender, &alice);
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(&original).unwrap();
        let mut replayer = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        replayer.encrypt(b"burn counter 0").unwrap();
        let moved = replayer.encrypt(&plain).unwrap();
        assert_eq!(receive(&moved, &alice.public()), None);
    }
}
