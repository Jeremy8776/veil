//! Sealed invites for short links: the node's shelf, and both clients of it.
//!
//! Purpose: a host leaves its sealed invite here ([`store_invite`]) and a guest holding the
//! short link fetches it ([`fetch_invite`]). See `veil_security::short_invite`.
//!
//! Invariants:
//! - Only a room's host can store, under its own open room, and on a link of its own, so a
//!   server too old to know the message closes that link and not the host's registered one.
//! - The shelf is bounded per room and overall, and a room's invites go when its host leaves.
//! - An id cannot be overwritten, so nobody can replace a stored invite.
//! - A guest dials with a throwaway key and does not check the server's: a short link has no
//!   room for the key, and the invite proves itself when it opens. So a stranger at a link's
//!   address learns only that someone dialled it, and can do no more than withhold the invite.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::task::JoinSet;
use veil_net::Node;
use veil_security::invite::Invite;
use veil_security::short_invite::{ID_LEN, MAX_SEALED, SealedInvite, ShortInvite};
use veil_security::{Identity, PublicId, SecurityError};

use crate::protocol::MAX_PASSES;
use crate::reach::any_addr;
use crate::room::RoomKey;
use crate::{RendezvousClient, RendezvousError};

/// Invites one room may have stored: as many as it may have lobby passes open.
pub const MAX_PER_ROOM: usize = MAX_PASSES;
/// Sealed bytes one server holds for all rooms together.
pub const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
/// Invites one link may ask for. A guest needs one.
pub const MAX_FETCHES_PER_LINK: u32 = 8;
/// How long a guest waits for an invite: a dial and an answer.
const FETCH_LIMIT: Duration = Duration::from_secs(10);

type Id = [u8; ID_LEN];

#[derive(Default)]
pub(crate) struct Shelf {
    entries: HashMap<Id, Vec<u8>>,
    /// Each room's ids, oldest first.
    rooms: HashMap<RoomKey, VecDeque<Id>>,
    bytes: usize,
}

impl Shelf {
    /// Stores an invite, dropping the room's oldest if it is full. False if refused.
    pub(crate) fn put(&mut self, room: RoomKey, id: Id, sealed: Vec<u8>) -> bool {
        if sealed.is_empty() || sealed.len() > MAX_SEALED || self.entries.contains_key(&id) {
            return false;
        }
        if self.rooms.get(&room).is_some_and(|ids| ids.len() >= MAX_PER_ROOM) {
            let oldest = self.rooms.get_mut(&room).and_then(VecDeque::pop_front);
            if let Some(oldest) = oldest {
                self.remove(&oldest);
            }
        }
        if self.bytes + sealed.len() > MAX_TOTAL_BYTES {
            return false;
        }
        self.bytes += sealed.len();
        self.rooms.entry(room).or_default().push_back(id);
        self.entries.insert(id, sealed);
        true
    }

    pub(crate) fn get(&self, id: &Id) -> Option<Vec<u8>> {
        self.entries.get(id).cloned()
    }

    /// Forgets everything `room` stored.
    pub(crate) fn drop_room(&mut self, room: &RoomKey) {
        for id in self.rooms.remove(room).unwrap_or_default() {
            self.remove(&id);
        }
    }

    fn remove(&mut self, id: &Id) {
        if let Some(sealed) = self.entries.remove(id) {
            self.bytes -= sealed.len();
        }
    }
}

/// Leaves a sealed invite on the server, in the room `room_tag` this node hosts.
pub async fn store_invite(
    node: &Node,
    addrs: &[SocketAddr],
    server: PublicId,
    room_tag: [u8; 32],
    sealed: SealedInvite,
) -> Result<(), RendezvousError> {
    let mut client = RendezvousClient::connect(node, addrs, server).await?;
    let stored = client.put_invite(room_tag, sealed).await;
    client.close();
    stored
}

/// Why a short link gave no invite. Worst last, so the most useful of several answers is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FetchError {
    /// No address answered.
    Unreachable,
    /// A server answered but does not have the invite.
    NotFound,
    /// The invite came back but cannot be used.
    Refused(SecurityRefusal),
}

/// Why a fetched invite was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecurityRefusal {
    Invalid,
    Expired,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unreachable => {
                "Could not reach the call for this invite link. The call may have ended, or \
                 your network may block it: ask the host for the full link."
            }
            Self::NotFound => "This invite link is no longer available. Ask the host for a new one.",
            Self::Refused(SecurityRefusal::Invalid) => "This invite link does not work.",
            Self::Refused(SecurityRefusal::Expired) => {
                "This invite link has expired. Ask the host for a new one."
            }
        })
    }
}

impl std::error::Error for FetchError {}

/// Asks every address in the link at once and returns the first invite that opens, so a stranger
/// at one address cannot get in the way of the real node at another.
pub async fn fetch_invite(short: &ShortInvite, now_unix: u64) -> Result<Invite, FetchError> {
    let id = short.id().map_err(|_| FetchError::Refused(SecurityRefusal::Invalid))?;
    let mut attempts = JoinSet::new();
    for &server in short.servers() {
        attempts.spawn(tokio::time::timeout(FETCH_LIMIT, fetch_from(server, id)));
    }
    let mut worst = FetchError::Unreachable;
    while let Some(attempt) = attempts.join_next().await {
        let outcome = match attempt {
            Ok(Ok(Ok(Some(sealed)))) => match short.open(&sealed, now_unix) {
                Ok(invite) => return Ok(invite),
                Err(SecurityError::Expired) => FetchError::Refused(SecurityRefusal::Expired),
                Err(_) => FetchError::Refused(SecurityRefusal::Invalid),
            },
            Ok(Ok(Ok(None))) => FetchError::NotFound,
            _ => FetchError::Unreachable,
        };
        worst = worst.max(outcome);
    }
    Err(worst)
}

async fn fetch_from(server: SocketAddr, id: Id) -> Result<Option<Vec<u8>>, RendezvousError> {
    let node = Node::bind(&Identity::generate(), any_addr(server))?;
    let link = node.connect_unverified(&[server]).await?;
    let mut client = RendezvousClient::attach(link).await?;
    let sealed = client.get_invite(id).await;
    client.close();
    sealed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(n: u8) -> RoomKey {
        ([n; 32], PublicId([n; 32]))
    }

    fn id(n: usize) -> Id {
        let mut id = [0; ID_LEN];
        id[..8].copy_from_slice(&n.to_be_bytes());
        id
    }

    #[test]
    fn a_stored_invite_comes_back_and_cannot_be_replaced() {
        let mut shelf = Shelf::default();
        assert!(shelf.put(room(1), id(1), vec![1, 2, 3]));
        assert!(!shelf.put(room(2), id(1), vec![9]), "another room's write");
        assert!(!shelf.put(room(1), id(1), vec![9]), "the same room's write");
        assert_eq!(shelf.get(&id(1)), Some(vec![1, 2, 3]));
        assert_eq!(shelf.get(&id(2)), None);
    }

    #[test]
    fn empty_or_oversized_invites_are_refused() {
        let mut shelf = Shelf::default();
        assert!(!shelf.put(room(1), id(1), Vec::new()));
        assert!(!shelf.put(room(1), id(2), vec![0; MAX_SEALED + 1]));
        assert!(shelf.put(room(1), id(3), vec![0; MAX_SEALED]));
    }

    #[test]
    fn a_full_room_drops_its_own_oldest_only() {
        let mut shelf = Shelf::default();
        assert!(shelf.put(room(2), id(10_000), vec![2]));
        for n in 0..=MAX_PER_ROOM {
            assert!(shelf.put(room(1), id(n), vec![1]));
        }
        assert_eq!(shelf.get(&id(0)), None, "the oldest went");
        assert!(shelf.get(&id(1)).is_some());
        assert!(shelf.get(&id(10_000)).is_some(), "another room is untouched");
        assert_eq!(shelf.bytes, MAX_PER_ROOM + 1);
    }

    #[test]
    fn the_whole_shelf_is_bounded() {
        let mut shelf = Shelf::default();
        let mut stored = 0;
        for n in 0..=MAX_TOTAL_BYTES / MAX_SEALED {
            let mut key = room(0);
            key.0[..8].copy_from_slice(&n.to_be_bytes());
            stored += usize::from(shelf.put(key, id(n), vec![0; MAX_SEALED]));
        }
        assert_eq!(stored, MAX_TOTAL_BYTES / MAX_SEALED);
        assert!(shelf.bytes <= MAX_TOTAL_BYTES);
    }

    #[test]
    fn leaving_clears_the_room_and_frees_its_bytes() {
        let mut shelf = Shelf::default();
        assert!(shelf.put(room(1), id(1), vec![1; 10]));
        assert!(shelf.put(room(2), id(2), vec![2; 5]));
        shelf.drop_room(&room(1));
        assert_eq!(shelf.get(&id(1)), None);
        assert!(shelf.get(&id(2)).is_some());
        assert_eq!(shelf.bytes, 5);
    }

    #[test]
    fn the_most_useful_failure_wins() {
        let expired = FetchError::Refused(SecurityRefusal::Expired);
        assert!(FetchError::Unreachable < FetchError::NotFound);
        assert!(FetchError::NotFound < expired);
    }
}
