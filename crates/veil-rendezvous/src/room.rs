//! Who may be in a rendezvous room.
//!
//! Purpose: a room is named by its tag and its host's key ([`RoomKey`]). The host opens it and
//! keeps two lists: keys it allows (members, and joiners in its lobby) and lobby passes it has
//! open (`veil_security::invite::lobby_pass` of each unused invite). Anyone else is refused.
//!
//! Invariants:
//! - The host is always entitled to its own room.
//! - A pass admits one key at a time. A key that gets onto the allow list gives its pass up.
//! - [`Room::allow`] replaces both lists and drops every member they no longer cover, so a
//!   declined joiner or removed member leaves the room with its pass or listing.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use tokio::sync::mpsc;
use veil_security::PublicId;

use crate::protocol::ServerMsg;

pub(crate) struct Member {
    pub addrs: Vec<SocketAddr>,
    pub outbox: mpsc::Sender<ServerMsg>,
    pub conn: quinn::Connection,
    /// The lobby pass this member registered with, if it was not on the allow list.
    pub pass: Option<[u8; 32]>,
}

/// A room is named by its tag and its host's key.
pub(crate) type RoomKey = ([u8; 32], PublicId);

pub(crate) struct Room {
    pub members: HashMap<PublicId, Member>,
    /// Keys the host allows, besides itself.
    pub allowed: HashSet<PublicId>,
    /// Lobby passes the host has open.
    pub passes: HashSet<[u8; 32]>,
}

impl Room {
    pub fn new() -> Self {
        Self {
            members: HashMap::new(),
            allowed: HashSet::new(),
            passes: HashSet::new(),
        }
    }

    /// Whether `key` may stay in this room, hosted by `host`.
    pub fn entitled(&self, host: PublicId, key: PublicId, pass: Option<&[u8; 32]>) -> bool {
        key == host || self.allowed.contains(&key) || pass.is_some_and(|p| self.passes.contains(p))
    }

    /// Whether another member already registered with `pass`.
    pub fn pass_taken(&self, pass: &[u8; 32]) -> bool {
        self.members.values().any(|m| m.pass.as_ref() == Some(pass))
    }

    /// Replaces the allow list and open passes, and drops every member they no longer
    /// cover. Returns the dropped keys, whose sessions and requests the caller forgets.
    pub fn allow(
        &mut self,
        host: PublicId,
        members: Vec<PublicId>,
        passes: Vec<[u8; 32]>,
    ) -> Vec<PublicId> {
        self.allowed = members.into_iter().collect();
        self.passes = passes.into_iter().collect();
        // A member now on the allow list no longer needs its pass, which frees the pass.
        for (device, member) in &mut self.members {
            if self.allowed.contains(device) {
                member.pass = None;
            }
        }
        let dropped: Vec<PublicId> = self
            .members
            .iter()
            .filter(|(device, m)| !self.entitled(host, **device, m.pass.as_ref()))
            .map(|(device, _)| *device)
            .collect();
        for device in &dropped {
            self.members.remove(device);
        }
        dropped
    }
}
