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
//! - The host's gate is on by default: every knock waits for the host. With it off
//!   ([`Lobby::set_gate`]), a knock that passes every check is approved at once. The checks
//!   stay the same either way: the room, a single-use unexpired token, the seat limit, no
//!   duplicate device, and the key package signed by the device that knocked. Joiners already
//!   waiting when the gate goes off keep waiting for the host.
//! - Each unused, unexpired token has a lobby pass ([`Lobby::passes`]) the host gives the
//!   rendezvous node, so joiners can register there before they knock. Burning the token
//!   closes its pass.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use veil_net::control::{Control, RefuseReason};
use veil_security::PublicId;
use veil_security::invite::{Invite, lobby_pass};

use crate::GroupError;
use crate::group::{Admission, Group, MAX_MEMBERS};

/// A joiner whose token checked out, waiting for the host's decision.
#[derive(Debug, PartialEq, Eq)]
pub struct Knock {
    pub device: PublicId,
}

/// A joiner who disconnects while waiting must be [`Lobby::forget`]-ed, or it keeps its seat.
pub struct Lobby {
    room_id: [u8; 16],
    /// Token hash to expiry (Unix seconds) and the token's lobby pass.
    tokens: BTreeMap<[u8; 32], (u64, [u8; 32])>,
    waiting: BTreeSet<PublicId>,
    approved: BTreeSet<PublicId>,
    /// Whether each knock waits for the host's decision. On unless the host turned it off.
    gate: bool,
}

/// What became of a knock that passed every check.
#[derive(Debug, PartialEq, Eq)]
pub enum Knocked {
    /// The joiner waits. Answer with [`Lobby::approve`] or [`Lobby::decline`].
    Waiting(Knock),
    /// The gate is off: the joiner is approved. Reply with the returned `Control::Admitted`.
    /// The join token is burnt all the same, so the host should give out a fresh invite.
    Approved(PublicId, Control),
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
            gate: true,
        }
    }

    /// Whether each knock waits for the host's decision.
    pub fn gate(&self) -> bool {
        self.gate
    }

    /// Turns the host's gate on or off. Only who decides changes; no check is skipped.
    pub fn set_gate(&mut self, on: bool) {
        self.gate = on;
    }

    pub fn room_id(&self) -> [u8; 16] {
        self.room_id
    }

    /// Registers an invite the host made for this room.
    pub fn add_invite(&mut self, invite: &Invite) -> Result<(), GroupError> {
        if invite.room_id != self.room_id {
            return Err(GroupError::Refused(RefuseReason::UnknownRoom));
        }
        self.tokens.insert(
            token_hash(&invite.join_token),
            (invite.expires_unix, lobby_pass(&invite.join_token)),
        );
        Ok(())
    }

    /// Lobby passes of the tokens still open at `now_unix`, at most `max`, the latest
    /// expiring first. Expired tokens are forgotten here.
    pub fn passes(&mut self, now_unix: u64, max: usize) -> Vec<[u8; 32]> {
        self.tokens.retain(|_, (expires, _)| now_unix < *expires);
        let mut open: Vec<(u64, [u8; 32])> = self.tokens.values().copied().collect();
        open.sort_by_key(|(expires, _)| std::cmp::Reverse(*expires));
        open.into_iter().take(max).map(|(_, pass)| pass).collect()
    }

    /// Joiners waiting for the host or approved and not yet in the group.
    pub fn seated(&self) -> Vec<PublicId> {
        self.waiting.iter().chain(&self.approved).copied().collect()
    }

    /// Checks a joiner's `Hello`. `device` is the key the QUIC link was pinned to. Pass the
    /// knock to [`Lobby::at_gate`] next.
    pub fn knock(
        &mut self,
        hello: &Control,
        device: PublicId,
        group: &Group,
        now_unix: u64,
    ) -> Result<Knock, GroupError> {
        let Control::Hello {
            room_id,
            join_token,
        } = hello
        else {
            return Err(GroupError::Unexpected("hello"));
        };
        if *room_id != self.room_id {
            return Err(GroupError::Refused(RefuseReason::UnknownRoom));
        }
        let (expires, _) = self
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

    /// Decides who answers a checked knock. With the gate on it waits for the host; with it
    /// off it is approved now. Either way it already passed every check in [`Lobby::knock`].
    pub fn at_gate(&mut self, knock: Knock) -> Knocked {
        if self.gate {
            return Knocked::Waiting(knock);
        }
        let device = knock.device;
        match self.approve(knock) {
            Ok(reply) => Knocked::Approved(device, reply),
            // Not waiting (already forgotten): leave it to the host, who will find nothing.
            Err(_) => Knocked::Waiting(Knock { device }),
        }
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

    /// Frees the seat of a joiner whose link closed before it joined the group.
    pub fn forget(&mut self, device: &PublicId) {
        self.waiting.remove(device);
        self.approved.remove(device);
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
