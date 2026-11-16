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
//! - Time is passed in, so the grace period is tested without sleeping.

use std::time::{Duration, Instant};

use veil_security::{PublicId, SecurityError};
use veil_security::frame::{FrameReceiver, HEADER_LEN};

/// How long the previous epoch's frames are still accepted after a change.
pub const GRACE: Duration = Duration::from_secs(2);

struct Previous {
    epoch: u64,
    receiver: FrameReceiver,
    until: Instant,
}

/// Opens frames from the current epoch and the previous one during its grace period.
pub struct KeyRing {
    epoch: u64,
    receiver: FrameReceiver,
    /// The current epoch's devices, to see who an epoch change removed.
    members: Vec<PublicId>,
    previous: Option<Previous>,
}

/// The epoch a sealed frame claims, from its key id. `None` if the packet is too short.
pub fn frame_epoch(packet: &[u8]) -> Option<u64> {
    if packet.len() < HEADER_LEN {
        return None;
    }
    let kid: [u8; 8] = packet.get(..8)?.try_into().ok()?;
    Some(u64::from_be_bytes(kid) >> 16)
}

impl KeyRing {
    /// `members` is the epoch's roster of devices.
    pub fn new(epoch: u64, receiver: FrameReceiver, members: impl IntoIterator<Item = PublicId>) -> Self {
        Self {
            epoch,
            receiver,
            members: members.into_iter().collect(),
            previous: None,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Moves to a new epoch at `now`, with that epoch's roster of devices. The old receiver
    /// stays open until `now + GRACE`, unless a device in the old roster is not in the new
    /// one: then it closes now. An epoch that is not newer than the current one is ignored.
    pub fn rotate(
        &mut self,
        epoch: u64,
        receiver: FrameReceiver,
        members: impl IntoIterator<Item = PublicId>,
        now: Instant,
    ) {
        if epoch <= self.epoch {
            return;
        }
        let members: Vec<PublicId> = members.into_iter().collect();
        let someone_left = self.members.iter().any(|m| !members.contains(m));
        let old = std::mem::replace(&mut self.receiver, receiver);
        self.previous = (!someone_left).then(|| Previous {
            epoch: self.epoch,
            receiver: old,
            until: now + GRACE,
        });
        self.epoch = epoch;
        self.members = members;
    }

    /// Returns the sender index and plaintext. Refuses any epoch other than the current one,
    /// or the previous one before its grace ends.
    pub fn open(&mut self, packet: &[u8], now: Instant) -> Result<(u32, Vec<u8>), SecurityError> {
        let epoch = frame_epoch(packet).ok_or(SecurityError::Malformed("short packet"))?;
        if epoch == self.epoch {
            return self.receiver.decrypt(packet);
        }
        if self.previous.as_ref().is_some_and(|p| now >= p.until) {
            self.previous = None;
        }
        match self.previous.as_mut() {
            Some(previous) if previous.epoch == epoch => previous.receiver.decrypt(packet),
            _ => Err(SecurityError::UnknownKey),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_security::frame::FrameSender;

    use super::*;

    fn devices(ids: &[u8]) -> Vec<PublicId> {
        ids.iter().map(|i| PublicId([*i; 32])).collect()
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
        let mut ring = KeyRing::new(1, old_receiver, devices(&[1, 2]));
        let late = old_sender.encrypt(b"sealed before the change").unwrap();
        let later = old_sender.encrypt(b"sealed long before").unwrap();
        ring.rotate(2, new_receiver, devices(&[1, 2, 3]), start);

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
        let mut ring = KeyRing::new(1, r1, devices(&[1]));
        let packet = first.encrypt(b"epoch one").unwrap();
        ring.rotate(2, r2, devices(&[1, 2]), now);
        ring.rotate(3, r3, devices(&[1, 2, 3]), now);
        assert_eq!(ring.epoch(), 3);
        assert_eq!(ring.open(&packet, now), Err(SecurityError::UnknownKey));
    }

    #[test]
    fn older_or_equal_epochs_do_not_rotate() {
        let now = Instant::now();
        let (mut sender, r2) = keys(2);
        let (_, r1) = keys(1);
        let mut ring = KeyRing::new(2, r2, devices(&[1, 2]));
        ring.rotate(1, r1, devices(&[1]), now);
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
        let mut ring = KeyRing::new(1, r1, devices(&[1, 2, 3]));
        let injected = removed.encrypt(b"sealed after removal").unwrap();
        ring.rotate(2, r2, devices(&[1, 2]), now);
        assert_eq!(ring.open(&injected, now), Err(SecurityError::UnknownKey));
        assert_eq!(ring.open(&stays.encrypt(b"new epoch").unwrap(), now).unwrap().1, b"new epoch");
    }

    #[test]
    fn a_swap_of_one_device_for_another_counts_as_a_removal() {
        let now = Instant::now();
        let (mut old, r1) = keys(1);
        let (_, r2) = keys(2);
        let mut ring = KeyRing::new(1, r1, devices(&[1, 2]));
        let packet = old.encrypt(b"epoch one").unwrap();
        ring.rotate(2, r2, devices(&[1, 3]), now);
        assert_eq!(ring.open(&packet, now), Err(SecurityError::UnknownKey));
    }
}
