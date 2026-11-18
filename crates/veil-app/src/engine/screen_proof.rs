//! Proof of who is sharing: every picture of a shared screen is signed by the sender's device.
//!
//! Purpose: all members hold the epoch secret, so the frame key cannot say which member sealed
//! a frame. Through a forwarder a member could otherwise show any picture on everyone's stage
//! as "Alice is sharing". The sender signs each screen picture with its device key, and the
//! receiver checks that signature against the device the roster gives for the frame's leaf.
//!
//! Wire format of a screen frame's plaintext: the 27-byte video header, a 64-byte Ed25519
//! signature, then the bitstream. The signature covers a domain tag, the epoch's binding, the
//! frame id, the layer marker, the header and the bitstream.
//!
//! Invariants:
//! - A frame marked as a screen reaches the decoder only with a valid signature. There is no
//!   unsigned form, so leaving the signature off does not help.
//! - The frame id and binding tie a signature to one frame of one call and epoch, so a signed
//!   picture cannot be moved to another counter, sender or call.
//! - Forged frames can still move the media replay window, as they can for camera and sound
//!   (`docs/TRANSPARENCY.md`). They can stop a real picture from showing, not replace it.

use veil_media::VideoHeader;
use veil_media::payload::VIDEO_HEADER_LEN;
use veil_security::frame::{BINDING_LEN, ID_LEN, frame_id};
use veil_security::layer::Layer;
use veil_security::{Identity, PublicId};

const DOMAIN: &[u8] = b"veil-screen-frame-v1";
const SIGNATURE_LEN: usize = 64;

/// True if `payload` starts with a valid video header marked as a screen.
pub fn is_screen(payload: &[u8]) -> bool {
    VideoHeader::read(payload).is_ok_and(|(header, _)| header.screen)
}

fn signed_message(
    binding: &[u8; BINDING_LEN],
    id: &[u8; ID_LEN],
    layer: Layer,
    head: &[u8],
    bitstream: &[u8],
) -> Vec<u8> {
    let marker = layer.encode();
    let parts = [DOMAIN, binding.as_slice(), id.as_slice(), &marker, head, bitstream];
    parts.concat()
}

/// `payload` with the signature for the frame `id` added, ready to seal under `layer`.
pub fn sign(
    identity: &Identity,
    binding: &[u8; BINDING_LEN],
    id: &[u8; ID_LEN],
    layer: Layer,
    payload: &[u8],
) -> Option<Vec<u8>> {
    let (head, bitstream) = payload.split_at_checked(VIDEO_HEADER_LEN)?;
    let signature = identity.sign(&signed_message(binding, id, layer, head, bitstream));
    Some([head, signature.as_slice(), bitstream].concat())
}

/// The opened frame without its signature, if `device` signed it for this very frame. `packet`
/// is the sealed frame as it arrived and `layer` the marker read from it.
pub fn check(
    device: &PublicId,
    binding: &[u8; BINDING_LEN],
    packet: &[u8],
    layer: Layer,
    plain: Vec<u8>,
) -> Option<Vec<u8>> {
    let id = frame_id(packet)?;
    let (head, rest) = plain.split_at_checked(VIDEO_HEADER_LEN)?;
    let (signature, bitstream) = rest.split_first_chunk::<SIGNATURE_LEN>()?;
    let message = signed_message(binding, &id, layer, head, bitstream);
    device.verify(&message, signature).ok()?;
    let mut payload = plain;
    payload.drain(VIDEO_HEADER_LEN..VIDEO_HEADER_LEN + SIGNATURE_LEN);
    Some(payload)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_media::Codec;
    use veil_security::frame::{FrameReceiver, FrameSender};

    use super::*;

    const SECRET: [u8; 32] = [7; 32];
    const EPOCH: u64 = 3;
    const LEAF: u32 = 2;

    fn picture() -> Vec<u8> {
        let header = VideoHeader {
            codec: Codec::H264,
            picture: 5,
            reference: None,
            needs_lower: false,
            screen: true,
            timestamp_us: 1,
            width: 16,
            height: 16,
        };
        header.write(b"bitstream").unwrap()
    }

    fn seal(sender: &mut FrameSender, signer: &Identity) -> Vec<u8> {
        let (binding, id) = (*sender.binding(), sender.next_frame_id());
        let signed = sign(signer, &binding, &id, Layer::BASE, &picture()).unwrap();
        sender.encrypt_layer(Layer::BASE, &signed).unwrap()
    }

    fn receive(sealed: &[u8], secret: &[u8; 32], device: &PublicId) -> Option<Vec<u8>> {
        let mut receiver = FrameReceiver::new(secret, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(sealed).unwrap();
        check(device, receiver.binding(), sealed, Layer::BASE, plain)
    }

    #[test]
    fn a_signed_picture_arrives_as_it_was_encoded() {
        let alice = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let sealed = seal(&mut sender, &alice);
        assert_eq!(receive(&sealed, &SECRET, &alice.public()), Some(picture()));
        assert!(is_screen(&picture()));
    }

    #[test]
    fn a_member_cannot_share_as_another() {
        let (alice, mallory) = (Identity::generate(), Identity::generate());
        let mut as_alice = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let forged = seal(&mut as_alice, &mallory);
        assert_eq!(receive(&forged, &SECRET, &alice.public()), None);
    }

    #[test]
    fn a_frame_without_a_signature_is_refused() {
        let alice = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let unsigned = sender.encrypt_layer(Layer::BASE, &picture()).unwrap();
        assert_eq!(receive(&unsigned, &SECRET, &alice.public()), None);
    }

    #[test]
    fn a_signed_picture_cannot_move_to_another_frame_or_call() {
        let alice = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let original = seal(&mut sender, &alice);
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(&original).unwrap();

        // Another counter, under the same leaf.
        let mut replayer = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        replayer.encrypt(b"burn counter 0").unwrap();
        let moved = replayer.encrypt_layer(Layer::BASE, &plain).unwrap();
        assert_eq!(receive(&moved, &SECRET, &alice.public()), None);

        // The same epoch, leaf and counter in another call.
        let other = [8; 32];
        let replayed = FrameSender::new(&other, EPOCH, LEAF)
            .unwrap()
            .encrypt_layer(Layer::BASE, &plain)
            .unwrap();
        assert_eq!(receive(&replayed, &other, &alice.public()), None);
    }

    #[test]
    fn a_changed_picture_or_layer_is_refused() {
        let alice = Identity::generate();
        let mut sender = FrameSender::new(&SECRET, EPOCH, LEAF).unwrap();
        let sealed = seal(&mut sender, &alice);
        let mut receiver = FrameReceiver::new(&SECRET, EPOCH).unwrap();
        let (_, plain) = receiver.decrypt(&sealed).unwrap();
        let binding = *receiver.binding();
        let device = alice.public();
        let mut changed = plain.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(check(&device, &binding, &sealed, Layer::BASE, changed), None);
        let layer = Layer {
            temporal: 1,
            ..Layer::BASE
        };
        assert_eq!(check(&device, &binding, &sealed, layer, plain), None);
    }
}
