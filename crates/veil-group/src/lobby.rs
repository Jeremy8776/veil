//! The host's lobby: checks invite tokens, waits for the host's decision, then hands approved
//! joiners to the group.
//!
//! Flow, all on the joiner's pinned QUIC control stream:
//! 1. Joiner sends `Control::Hello { room_id, join_token }`. [`Lobby::knock`] checks the room,
//!    the token and its expiry, and burns the token whatever the host decides.
//! 2. The host approves ([`Lobby::approve`], reply `Control::Admitted`) or declines
//!    ([`Lobby::decline`], reply `Control::Refused(DeclinedByHost)`).
//! 3. Joiner sends `Control::KeyPackage`. [`Lobby::admit`] checks the device was approved and
//!    adds it to the group, which starts a new epoch.
//!
//! Invariants:
//! - A token admits at most one knock. Tokens are stored as SHA-256 hashes, so lookups do not
//!   compare secret bytes and a memory dump shows no live tokens.
//! - An unknown token and a burnt one get the same answer (`TokenUsed`), so a guesser learns
//!   nothing about which tokens exist.
//! - The device that knocked is the device that must sign the key package.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use veil_net::control::{Control, RefuseReason};
use veil_security::PublicId;
use veil_security::invite::Invite;

use crate::group::{Admission, Group, MAX_MEMBERS};
use crate::GroupError;

/// A joiner whose token checked out, waiting for the host's decision.
#[derive(Debug, PartialEq, Eq)]
pub struct Knock {
    pub device: PublicId,
}

/// shortcut: a joiner who knocks or is approved and then disconnects keeps its seat until the
/// lobby is dropped. Upgrade with a timeout, or a `leave(device)` call on link close, when
/// `veil-node` drives the lobby from real connections.
pub struct Lobby {
    room_id: [u8; 16],
    /// Token hash to expiry (Unix seconds).
    tokens: BTreeMap<[u8; 32], u64>,
    waiting: BTreeSet<PublicId>,
    approved: BTreeSet<PublicId>,
}

fn token_hash(token: &[u8; 16]) -> [u8; 32] {
    Sha256::digest(token).into()
}

impl Lobby {
    pub fn new(room_id: [u8; 16]) -> Self {
        Self {
            room_id,
            tokens: BTreeMap::new(),
            waiting: BTreeSet::new(),
            approved: BTreeSet::new(),
        }
    }

    /// Registers an invite the host made for this room.
    pub fn add_invite(&mut self, invite: &Invite) -> Result<(), GroupError> {
        if invite.room_id != self.room_id {
            return Err(GroupError::Refused(RefuseReason::UnknownRoom));
        }
        self.tokens
            .insert(token_hash(&invite.join_token), invite.expires_unix);
        Ok(())
    }

    /// Checks a joiner's `Hello`. `device` is the key the QUIC link was pinned to.
    pub fn knock(
        &mut self,
        hello: &Control,
        device: PublicId,
        group: &Group,
        now_unix: u64,
    ) -> Result<Knock, GroupError> {
        let Control::Hello { room_id, join_token } = hello else {
            return Err(GroupError::Unexpected("hello"));
        };
        if *room_id != self.room_id {
            return Err(GroupError::Refused(RefuseReason::UnknownRoom));
        }
        let expires = self
            .tokens
            .remove(&token_hash(join_token))
            .ok_or(GroupError::Refused(RefuseReason::TokenUsed))?;
        if now_unix >= expires {
            return Err(GroupError::Refused(RefuseReason::TokenUsed));
        }
        let seats_taken = group.members().len() + self.waiting.len() + self.approved.len();
        if seats_taken >= MAX_MEMBERS {
            return Err(GroupError::Refused(RefuseReason::RoomFull));
        }
        if group.members().iter().any(|(_, d)| *d == device)
            || self.waiting.contains(&device)
            || self.approved.contains(&device)
        {
            return Err(GroupError::AlreadyMember);
        }
        self.waiting.insert(device);
        Ok(Knock { device })
    }

    /// The host lets the joiner in. Reply with the returned `Control::Admitted`.
    pub fn approve(&mut self, knock: Knock) -> Result<Control, GroupError> {
        if !self.waiting.remove(&knock.device) {
            return Err(GroupError::NotMember);
        }
        self.approved.insert(knock.device);
        Ok(Control::Admitted)
    }

    /// The host turns the joiner away. Reply with the returned `Control::Refused`.
    pub fn decline(&mut self, knock: Knock) -> Control {
        self.waiting.remove(&knock.device);
        Control::Refused(RefuseReason::DeclinedByHost)
    }

    /// Adds an approved joiner from its `Control::KeyPackage`. Send the commit to existing
    /// members and the welcome to the joiner.
    pub fn admit(
        &mut self,
        group: &mut Group,
        device: &PublicId,
        message: &Control,
    ) -> Result<Admission, GroupError> {
        let Control::KeyPackage(key_package) = message else {
            return Err(GroupError::Unexpected("key package"));
        };
        if !self.approved.remove(device) {
            return Err(GroupError::NotMember);
        }
        group.add(key_package, device)
    }
}
