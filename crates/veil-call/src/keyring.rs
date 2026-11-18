//! Frame receivers for the current epoch and, briefly, the one before.
//!
//! Purpose: an epoch change reaches members at slightly different moments, so frames sealed
//! just before a change are still in flight when a receiver moves on. [`KeyRing`] keeps the
//! previous epoch's [`FrameReceiver`] for [`GRACE`] after a change, so those frames still open.
//!
//! Invariants:
//! - At most two epochs open at once: the current one, and the previous one until its grace
//!   ends.
//! - No grace when a member left. Every member holds the epoch secret and can derive any
//!   sender's key from it, so the old epoch cannot tell a removed member's frames from anyone
//!   else's. If any device in the old roster is missing from the new one, the old receiver is
//!   dropped at once (audit M1). Frames still in flight from those who stay are lost: a short
//!   glitch, and a keyframe request repairs video.
//! - The epoch is read from the frame's key id (`epoch << 16 | sender`, see
//!   `veil_security::frame`) only to pick a receiver. The receiver still authenticates it.
//! - Each epoch keeps its own roster (leaf index to device), because leaf numbers can differ
//!   between epochs.
//! - A frame that came over a link from a member of the frame's epoch must name that member's
//!   leaf, or it is refused before any replay window moves ([`KeyRing::open_from`]). Otherwise
//!   one member could seal under another's leaf and push that member's genuine frames out of
//!   the window (GHSA-rvhv-r5mr-2w3h). Frames through a forwarder (a link peer outside the
//!   roster) cannot be bound this way.
//! - Names and chat ([`KeyRing::open_app`]) use the signed-frame window, which moves only after
//!   the sender's device signature checked out, on every route.
//! - Time is passed in, so the grace period is tested without sleeping.

use std::time::{Duration, Instant};

use veil_security::{PublicId, SecurityError};
use veil_security::frame::{BINDING_LEN, FrameReceiver, HEADER_LEN};

use crate::app_msg::{AppError, AppMessage};

/// How long the previous epoch's frames are still accepted after a change.
pub const GRACE: Duration = Duration::from_secs(2);

/// One epoch's receiver and roster.
struct Epoch {
    epoch: u64,
    receiver: FrameReceiver,
    /// Leaf index and device key of every member in this epoch.
    roster: Vec<(u32, PublicId)>,
}

impl Epoch {
    fn device(&self, leaf: u32) -> Option<PublicId> {
        self.roster
            .iter()
            .find(|(l, _)| *l == leaf)
            .map(|(_, d)| *d)
    }

    /// Refuses a frame whose sender index is not the leaf of `via`, when `via` is a member.
    fn check_sender(&self, packet: &[u8], via: Option<&PublicId>) -> Result<(), SecurityError> {
        let Some(via) = via else { return Ok(()) };
        let Some(&(leaf, _)) = self.roster.iter().find(|(_, d)| d == via) else {
            return Ok(()); // A forwarder, or a link the roster does not name.
        };
        match frame_sender(packet) {
            Some(sender) if sender == leaf => Ok(()),
            Some(_) => Err(SecurityError::UnknownKey),
            None => Err(SecurityError::Malformed("short packet")),
        }
    }
}

/// Opens frames from the current epoch and the previous one during its grace period.
pub struct KeyRing {
    current: Epoch,
    previous: Option<(Epoch, Instant)>,
}

/// The epoch a sealed frame claims, from its key id. `None` if the packet is too short.
pub fn frame_epoch(packet: &[u8]) -> Option<u64> {
    if packet.len() < HEADER_LEN {
        return None;
    }
    let kid: [u8; 8] = packet.get(..8)?.try_into().ok()?;
    Some(u64::from_be_bytes(kid) >> 16)
}

/// The sender index a sealed frame claims, from its key id. Not authenticated.
fn frame_sender(packet: &[u8]) -> Option<u32> {
    if packet.len() < HEADER_LEN {
        return None;
    }
    let kid: [u8; 8] = packet.get(..8)?.try_into().ok()?;
    Some((u64::from_be_bytes(kid) & 0xFFFF) as u32)
}

impl KeyRing {
    /// `roster` is the epoch's leaf index and device key of every member (`EpochKeys::roster`).
    pub fn new(epoch: u64, receiver: FrameReceiver, roster: impl IntoIterator<Item = (u32, PublicId)>) -> Self {
        Self {
            current: Epoch {
                epoch,
                receiver,
                roster: roster.into_iter().collect(),
            },
            previous: None,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.current.epoch
    }

    /// Moves to a new epoch at `now`, with that epoch's roster. The old receiver stays open
    /// until `now + GRACE`, unless a device in the old roster is not in the new one: then it
    /// closes now. An epoch that is not newer than the current one is ignored.
    pub fn rotate(
        &mut self,
        epoch: u64,
        receiver: FrameReceiver,
        roster: impl IntoIterator<Item = (u32, PublicId)>,
        now: Instant,
    ) {
        if epoch <= self.current.epoch {
            return;
        }
        let roster: Vec<(u32, PublicId)> = roster.into_iter().collect();
        let someone_left = self
            .current
            .roster
            .iter()
            .any(|(_, m)| !roster.iter().any(|(_, d)| d == m));
        let old = std::mem::replace(
            &mut self.current,
            Epoch {
                epoch,
                receiver,
                roster,
            },
        );
        self.previous = (!someone_left).then_some((old, now + GRACE));
    }

    /// Returns the sender index and plaintext. Refuses any epoch other than the current one,
    /// or the previous one before its grace ends. Use [`Self::open_from`] when the link the
    /// frame came on is known.
    pub fn open(&mut self, packet: &[u8], now: Instant) -> Result<(u32, Vec<u8>), SecurityError> {
        self.open_inner(packet, None, now)
    }

    /// As [`Self::open`], and refuses a frame from a member's link that names another leaf.
    pub fn open_from(
        &mut self,
        packet: &[u8],
        via: &PublicId,
        now: Instant,
    ) -> Result<(u32, Vec<u8>), SecurityError> {
        self.open_inner(packet, Some(via), now)
    }

    /// Opens a name or chat message that came over the link to `via`: the frame key, then the
    /// device signature of the leaf the frame names in that frame's epoch. Returns the signing
    /// device and the message. Only a message that checks out moves the replay window.
    pub fn open_app(
        &mut self,
        sealed: &[u8],
        via: &PublicId,
        now: Instant,
    ) -> Result<(PublicId, AppMessage), AppError> {
        let epoch = self.select(sealed, now).map_err(|_| AppError::Unopened)?;
        epoch
            .check_sender(sealed, Some(via))
            .map_err(|_| AppError::Unopened)?;
        let frame = epoch
            .receiver
            .decrypt_signed(sealed)
            .map_err(|_| AppError::Unopened)?;
        let device = epoch.device(frame.sender).ok_or(AppError::Unopened)?;
        let message = AppMessage::open(sealed, &frame.plain, &device, epoch.receiver.binding())?;
        epoch
            .receiver
            .confirm_signed(&frame)
            .map_err(|_| AppError::Unopened)?;
        Ok((device, message))
    }

    /// The device holding `leaf` and the signature binding, in the epoch `packet` names, for
    /// checking a signature carried inside a frame that has already opened.
    pub fn signer(
        &mut self,
        packet: &[u8],
        leaf: u32,
        now: Instant,
    ) -> Option<(PublicId, [u8; BINDING_LEN])> {
        let epoch = self.select(packet, now).ok()?;
        Some((epoch.device(leaf)?, *epoch.receiver.binding()))
    }

    fn open_inner(
        &mut self,
        packet: &[u8],
        via: Option<&PublicId>,
        now: Instant,
    ) -> Result<(u32, Vec<u8>), SecurityError> {
        let epoch = self.select(packet, now)?;
        epoch.check_sender(packet, via)?;
        epoch.receiver.decrypt(packet)
    }

    /// The epoch a packet names, if it is still open.
    fn select(&mut self, packet: &[u8], now: Instant) -> Result<&mut Epoch, SecurityError> {
        let epoch = frame_epoch(packet).ok_or(SecurityError::Malformed("short packet"))?;
        if epoch == self.current.epoch {
            return Ok(&mut self.current);
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.previous = None;
        }
        match self.previous.as_mut() {
            Some((previous, _)) if previous.epoch == epoch => Ok(previous),
            _ => Err(SecurityError::UnknownKey),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_security::Identity;
    use veil_security::frame::FrameSender;

    use super::*;

    /// Device `i` at leaf `i` for each id.
    fn roster(ids: &[u8]) -> Vec<(u32, PublicId)> {
        ids.iter()
            .map(|i| (u32::from(*i), PublicId([*i; 32])))
            .collect()
    }

    fn keys(epoch: u64) -> (FrameSender, FrameReceiver) {
        let secret = [u8::try_from(epoch).unwrap(); 32];
        (
            FrameSender::new(&secret, epoch, 3).unwrap(),
            FrameReceiver::new(&secret, epoch).unwrap(),
        )
    }

    #[test]
    fn previous_epoch_opens_during_grace_only() {
        let start = Instant::now();
        let (mut old_sender, old_receiver) = keys(1);
        let (mut new_sender, new_receiver) = keys(2);
        let mut ring = KeyRing::new(1, old_receiver, roster(&[1, 2]));
        let late = old_sender.encrypt(b"sealed before the change").unwrap();
        let later = old_sender.encrypt(b"sealed long before").unwrap();
        ring.rotate(2, new_receiver, roster(&[1, 2, 3]), start);

        assert_eq!(ring.open(&late, start + GRACE / 2).unwrap(), (3, b"sealed before the change".to_vec()));
        let fresh = new_sender.encrypt(b"new epoch").unwrap();
        assert_eq!(ring.open(&fresh, start).unwrap().1, b"new epoch");
        assert_eq!(ring.open(&later, start + GRACE), Err(SecurityError::UnknownKey));
    }

    #[test]
    fn only_one_previous_epoch_is_kept() {
        let now = Instant::now();
        let (mut first, r1) = keys(1);
        let (_, r2) = keys(2);
        let (_, r3) = keys(3);
        let mut ring = KeyRing::new(1, r1, roster(&[1]));
        let packet = first.encrypt(b"epoch one").unwrap();
        ring.rotate(2, r2, roster(&[1, 2]), now);
        ring.rotate(3, r3, roster(&[1, 2, 3]), now);
        assert_eq!(ring.epoch(), 3);
        assert_eq!(ring.open(&packet, now), Err(SecurityError::UnknownKey));
    }

    #[test]
    fn older_or_equal_epochs_do_not_rotate() {
        let now = Instant::now();
        let (mut sender, r2) = keys(2);
        let (_, r1) = keys(1);
        let mut ring = KeyRing::new(2, r2, roster(&[1, 2]));
        ring.rotate(1, r1, roster(&[1]), now);
        assert_eq!(ring.epoch(), 2);
        let packet = sender.encrypt(b"still two").unwrap();
        assert_eq!(ring.open(&packet, now).unwrap().1, b"still two");
        assert!(ring.open(&[0; 4], now).is_err());
    }

    #[test]
    fn a_removal_closes_the_previous_epoch_at_once() {
        let now = Instant::now();
        let (mut removed, r1) = keys(1);
        let (mut stays, r2) = keys(2);
        let mut ring = KeyRing::new(1, r1, roster(&[1, 2, 3]));
        let injected = removed.encrypt(b"sealed after removal").unwrap();
        ring.rotate(2, r2, roster(&[1, 2]), now);
        assert_eq!(ring.open(&injected, now), Err(SecurityError::UnknownKey));
        assert_eq!(ring.open(&stays.encrypt(b"new epoch").unwrap(), now).unwrap().1, b"new epoch");
    }

    #[test]
    fn a_swap_of_one_device_for_another_counts_as_a_removal() {
        let now = Instant::now();
        let (mut old, r1) = keys(1);
        let (_, r2) = keys(2);
        let mut ring = KeyRing::new(1, r1, roster(&[1, 2]));
        let packet = old.encrypt(b"epoch one").unwrap();
        ring.rotate(2, r2, roster(&[1, 3]), now);
        assert_eq!(ring.open(&packet, now), Err(SecurityError::UnknownKey));
    }

    #[test]
    fn the_signer_comes_from_the_epoch_the_frame_names() {
        let now = Instant::now();
        let (mut old_sender, old_receiver) = keys(1);
        let (_, new_receiver) = keys(2);
        let binding = *old_sender.binding();
        let mut ring = KeyRing::new(1, old_receiver, [(3, PublicId([1; 32]))]);
        let packet = old_sender.encrypt(b"epoch one").unwrap();
        ring.rotate(2, new_receiver, [(5, PublicId([1; 32])), (3, PublicId([2; 32]))], now);
        assert_eq!(ring.signer(&packet, 3, now), Some((PublicId([1; 32]), binding)));
        assert_eq!(ring.signer(&packet, 4, now), None, "no such leaf");
        assert_eq!(ring.signer(&packet, 3, now + GRACE), None, "epoch closed");
    }

    /// GHSA-rvhv-r5mr-2w3h, mesh: member 2 seals under member 3's leaf on its own link.
    #[test]
    fn a_member_link_cannot_carry_another_members_frames() {
        let now = Instant::now();
        let (mut victim, receiver) = keys(1);
        let mut ring = KeyRing::new(1, receiver, roster(&[1, 2, 3]));
        let mut forger = FrameSender::new(&[1; 32], 1, 3).unwrap();
        let forged = forger.encrypt(b"forged").unwrap();
        assert_eq!(
            ring.open_from(&forged, &PublicId([2; 32]), now),
            Err(SecurityError::UnknownKey)
        );
        let genuine = victim.encrypt(b"genuine").unwrap();
        assert_eq!(
            ring.open_from(&genuine, &PublicId([3; 32]), now).unwrap(),
            (3, b"genuine".to_vec())
        );
        // Through a forwarder (not a member) the leaf cannot be checked.
        let relayed = victim.encrypt(b"relayed").unwrap();
        assert_eq!(
            ring.open_from(&relayed, &PublicId([9; 32]), now).unwrap().1,
            b"relayed"
        );
    }

    /// GHSA-rvhv-r5mr-2w3h, any route: a forged far-ahead frame under the victim's leaf does
    /// not stop the victim's signed chat.
    #[test]
    fn a_forged_frame_does_not_stop_a_members_chat() {
        let now = Instant::now();
        let victim = Identity::generate();
        let forwarder = PublicId([9; 32]);
        let secret = [1; 32];
        let mut ring = KeyRing::new(
            1,
            FrameReceiver::new(&secret, 1).unwrap(),
            [(0, PublicId([0; 32])), (3, victim.public())],
        );
        let mut forger = FrameSender::new(&secret, 1, 3).unwrap();
        for _ in 0..1_000 {
            forger.encrypt(b"burn").unwrap();
        }
        let forged = AppMessage::chat("not from the victim")
            .unwrap()
            .seal(&mut forger, &Identity::generate())
            .unwrap();
        assert_eq!(
            ring.open_app(&forged, &forwarder, now),
            Err(AppError::BadSignature)
        );
        let media = forger.encrypt(b"far ahead").unwrap();
        assert!(ring.open_from(&media, &forwarder, now).is_ok());

        let mut sender = FrameSender::new(&secret, 1, 3).unwrap();
        let message = AppMessage::chat("still heard").unwrap();
        let sealed = message.seal(&mut sender, &victim).unwrap();
        assert_eq!(
            ring.open_app(&sealed, &forwarder, now).unwrap(),
            (victim.public(), message)
        );
        assert_eq!(
            ring.open_app(&sealed, &forwarder, now),
            Err(AppError::Unopened),
            "once only"
        );
    }
}
