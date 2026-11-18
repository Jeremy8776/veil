//! The relay session table.
//!
//! Purpose: decide who may hold a relay session, so that keys, which cost nothing, cannot fill
//! the table and shut out other rooms.
//!
//! Invariants:
//! - A session forwards only between its two members.
//! - A session unused for [`SESSION_IDLE`] gives its place back when a limit is reached. Live
//!   relayed links send a keep-alive every few seconds, so they are never idle this long.
//! - Two members hold at most [`MAX_SESSIONS_PER_PAIR`] together. A further one replaces their
//!   session idle longest, so one asker cannot fill the sessions of a member that accepts
//!   anyone (the host).
//! - One remote address block (`veil_net::accept::block`) holds at most
//!   [`MAX_SESSIONS_PER_BLOCK`] sessions. Address blocks are the one thing a party cannot make
//!   for free. A full relay-only call from one address needs 120.
//! - When the table is full of active sessions, a newcomer from a smaller block takes the place
//!   of the longest-idle session of the block that holds the most.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use veil_security::PublicId;

pub const MAX_SESSIONS: usize = 4_096;
/// Relay sessions one member may hold. A call has at most 16 members.
pub const MAX_SESSIONS_PER_MEMBER: usize = 16;
/// Relay sessions two members may hold together. They need two (control and media); the rest
/// leave room for retries. This also means one member cannot fill another's sessions.
pub const MAX_SESSIONS_PER_PAIR: usize = 4;
/// Relay sessions the members in one remote address block may hold together.
pub const MAX_SESSIONS_PER_BLOCK: usize = MAX_SESSIONS / 32;
/// Longer than the 20 s after which a QUIC link with no traffic ends.
pub const SESSION_IDLE: Duration = Duration::from_secs(30);

/// One side of a session. Generic so the table can be tested without connections.
pub(crate) struct End<T> {
    pub key: PublicId,
    pub block: IpAddr,
    pub to: T,
}

struct Session<T> {
    a: End<T>,
    b: End<T>,
    last_used: Instant,
}

impl<T> Session<T> {
    fn other(&self, from: PublicId) -> Option<&T> {
        if from == self.a.key {
            Some(&self.b.to)
        } else if from == self.b.key {
            Some(&self.a.to)
        } else {
            None
        }
    }

    fn involves(&self, key: PublicId) -> bool {
        self.a.key == key || self.b.key == key
    }

    fn touches(&self, block: IpAddr) -> bool {
        self.a.block == block || self.b.block == block
    }

    fn idle(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_used) >= SESSION_IDLE
    }
}

pub(crate) struct Sessions<T> {
    map: HashMap<u32, Session<T>>,
    next: u32,
}

impl<T> Default for Sessions<T> {
    fn default() -> Self {
        Self { map: HashMap::new(), next: 0 }
    }
}

impl<T> Sessions<T> {
    fn count_of(&self, key: PublicId) -> usize {
        self.map.values().filter(|s| s.involves(key)).count()
    }

    fn count_in(&self, block: IpAddr) -> usize {
        self.map.values().filter(|s| s.touches(block)).count()
    }

    /// Whether `key` is at its limit, after reclaiming its idle sessions if it was.
    pub(crate) fn member_full(&mut self, key: PublicId, now: Instant) -> bool {
        if self.count_of(key) < MAX_SESSIONS_PER_MEMBER {
            return false;
        }
        self.map.retain(|_, s| !(s.involves(key) && s.idle(now)));
        self.count_of(key) >= MAX_SESSIONS_PER_MEMBER
    }

    fn block_full(&mut self, block: IpAddr, now: Instant) -> bool {
        if self.count_in(block) < MAX_SESSIONS_PER_BLOCK {
            return false;
        }
        self.map.retain(|_, s| !(s.touches(block) && s.idle(now)));
        self.count_in(block) >= MAX_SESSIONS_PER_BLOCK
    }

    /// Frees a place in a full table for a session between `blocks`, if that is fair to do.
    fn make_room(&mut self, blocks: [IpAddr; 2], now: Instant) -> bool {
        if self.map.len() < MAX_SESSIONS {
            return true;
        }
        self.map.retain(|_, s| !s.idle(now));
        if self.map.len() < MAX_SESSIONS {
            return true;
        }
        let mut counts: HashMap<IpAddr, usize> = HashMap::new();
        for s in self.map.values() {
            *counts.entry(s.a.block).or_default() += 1;
            if s.b.block != s.a.block {
                *counts.entry(s.b.block).or_default() += 1;
            }
        }
        let held = |block| counts.get(&block).copied().unwrap_or(0);
        let mine = held(blocks[0]).max(held(blocks[1]));
        let Some((&busiest, &most)) = counts.iter().max_by_key(|(_, count)| **count) else {
            return false;
        };
        if most <= mine {
            return false;
        }
        let oldest = self
            .map
            .iter()
            .filter(|(_, s)| s.touches(busiest))
            .min_by_key(|(_, s)| s.last_used)
            .map(|(id, _)| *id);
        oldest.is_some_and(|id| self.map.remove(&id).is_some())
    }

    /// Gives up the pair's session idle longest once the pair holds its limit.
    fn replace_in_pair(&mut self, x: PublicId, y: PublicId) {
        let pair = || self.map.iter().filter(|(_, s)| s.involves(x) && s.involves(y));
        if pair().count() < MAX_SESSIONS_PER_PAIR {
            return;
        }
        if let Some(id) = pair().min_by_key(|(_, s)| s.last_used).map(|(id, _)| *id) {
            self.map.remove(&id);
        }
    }

    /// Opens a session, or returns `None` if a limit refuses it.
    pub(crate) fn open(&mut self, a: End<T>, b: End<T>, now: Instant) -> Option<u32> {
        self.replace_in_pair(a.key, b.key);
        if self.member_full(a.key, now)
            || self.member_full(b.key, now)
            || self.block_full(a.block, now)
            || self.block_full(b.block, now)
            || !self.make_room([a.block, b.block], now)
        {
            return None;
        }
        let mut id = self.next;
        // Bounded: fewer than MAX_SESSIONS ids are taken, so a free one is found quickly.
        while self.map.contains_key(&id) {
            id = id.wrapping_add(1);
        }
        self.next = id.wrapping_add(1);
        self.map.insert(id, Session { a, b, last_used: now });
        Some(id)
    }

    /// Where a packet from `from` goes. Counts as use of the session.
    pub(crate) fn route(&mut self, id: u32, from: PublicId, now: Instant) -> Option<&T> {
        let session = self.map.get_mut(&id)?;
        if !session.involves(from) {
            return None;
        }
        session.last_used = now;
        session.other(from)
    }

    pub(crate) fn forget(&mut self, key: PublicId) {
        self.map.retain(|_, s| !s.involves(key));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::protocol;

    fn key(n: u32) -> PublicId {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&n.to_be_bytes());
        PublicId(bytes)
    }

    fn block(n: u32) -> IpAddr {
        IpAddr::V4(n.into())
    }

    fn end(k: u32, b: u32) -> End<char> {
        End { key: key(k), block: block(b), to: 'x' }
    }

    /// Fills `count` sessions inside `b`, each between two keys of their own.
    fn fill(t: &mut Sessions<char>, first_key: u32, b: u32, count: usize, now: Instant) {
        for i in 0..u32::try_from(count).unwrap() {
            let k = first_key + i * 2;
            t.open(end(k, b), end(k + 1, b), now).unwrap();
        }
    }

    #[test]
    fn session_routes_only_between_its_two_members() {
        let now = Instant::now();
        let mut t = Sessions::default();
        let a = End { key: key(1), block: block(1), to: 'A' };
        let b = End { key: key(2), block: block(2), to: 'B' };
        let id = t.open(a, b, now).unwrap();
        assert_eq!(t.route(id, key(1), now), Some(&'B'));
        assert_eq!(t.route(id, key(2), now), Some(&'A'));
        assert_eq!(t.route(id, key(3), now), None);
    }

    #[test]
    fn session_ids_reuse_protocol_framing() {
        let packet = protocol::session_packet(5, b"x");
        assert_eq!(protocol::split_session(&packet).map(|(id, _)| id), Some(5));
    }

    #[test]
    fn a_full_pair_gives_up_its_longest_idle_session_only() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut t = Sessions::default();
        let other_pair = t.open(end(1, 1), end(3, 3), start).unwrap();
        let first = t.open(end(1, 1), end(2, 2), at(1)).unwrap();
        let second = t.open(end(2, 2), end(1, 1), at(2)).unwrap();
        t.open(end(1, 1), end(2, 2), at(3)).unwrap();
        t.open(end(1, 1), end(2, 2), at(4)).unwrap();
        assert!(t.route(first, key(1), at(5)).is_some(), "still carrying a link");

        t.open(end(1, 1), end(2, 2), at(6)).unwrap();
        assert!(t.route(second, key(1), at(6)).is_none());
        assert!(t.route(first, key(1), at(6)).is_some());
        assert!(t.route(other_pair, key(1), at(6)).is_some());
    }

    #[test]
    fn one_block_cannot_hold_more_than_its_share() {
        let now = Instant::now();
        let mut t = Sessions::default();
        fill(&mut t, 0, 1, MAX_SESSIONS_PER_BLOCK, now);
        assert!(t.open(end(900_000, 1), end(900_001, 1), now).is_none());
        assert!(t.open(end(900_000, 1), end(900_001, 2), now).is_none());
        assert!(t.open(end(900_000, 2), end(900_001, 2), now).is_some());
    }

    #[test]
    fn a_full_relay_only_call_from_one_address_fits() {
        let now = Instant::now();
        let mut t = Sessions::default();
        for a in 0..16 {
            for b in (a + 1)..16 {
                t.open(end(a, 1), end(b, 1), now).unwrap();
            }
        }
        assert_eq!(t.count_in(block(1)), 120);
    }

    #[test]
    fn idle_sessions_give_their_places_back_and_used_ones_keep_them() {
        let start = Instant::now();
        let mut t = Sessions::default();
        fill(&mut t, 0, 1, MAX_SESSIONS_PER_BLOCK, start);
        let later = start + SESSION_IDLE;
        let kept = 0;
        assert!(t.route(kept, key(0), later - Duration::from_secs(1)).is_some());
        t.open(end(900_000, 1), end(900_001, 1), later).unwrap();
        assert_eq!(t.count_in(block(1)), 2);
        assert!(t.route(kept, key(0), later).is_some());
    }

    #[test]
    fn a_member_at_its_limit_gets_idle_places_back() {
        let start = Instant::now();
        let mut t = Sessions::default();
        for i in 1..=u32::try_from(MAX_SESSIONS_PER_MEMBER).unwrap() {
            t.open(end(0, 1), end(i, 2), start).unwrap();
        }
        assert!(t.member_full(key(0), start));
        assert!(!t.member_full(key(0), start + SESSION_IDLE));
    }

    #[test]
    fn a_table_full_of_active_sessions_makes_way_for_a_smaller_block() {
        let now = Instant::now();
        let mut t = Sessions::default();
        let share = u32::try_from(MAX_SESSIONS_PER_BLOCK).unwrap();
        for b in 0..32 {
            let count = if b == 0 { MAX_SESSIONS_PER_BLOCK } else { MAX_SESSIONS_PER_BLOCK - 1 };
            fill(&mut t, (b + 1) * share * 4, b + 1, count, now);
        }
        // 31 places are still free. Fill them from a block of their own.
        let free = MAX_SESSIONS - t.map.len();
        fill(&mut t, 900_000, 100, free, now);
        assert_eq!(t.map.len(), MAX_SESSIONS);
        let newcomer = t.open(end(990_000, 200), end(990_001, 200), now).unwrap();
        assert_eq!(t.map.len(), MAX_SESSIONS);
        assert!(t.route(newcomer, key(990_000), now).is_some());
        assert_eq!(t.count_in(block(1)), MAX_SESSIONS_PER_BLOCK - 1);
    }

    #[test]
    fn the_busiest_block_cannot_evict_others_when_the_table_is_full() {
        let now = Instant::now();
        let mut t = Sessions::default();
        let share = u32::try_from(MAX_SESSIONS_PER_BLOCK).unwrap();
        for b in 0..32 {
            let count = if b == 0 { MAX_SESSIONS_PER_BLOCK - 1 } else { MAX_SESSIONS_PER_BLOCK };
            fill(&mut t, (b + 1) * share * 4, b + 1, count, now);
        }
        let free = MAX_SESSIONS - t.map.len();
        fill(&mut t, 900_000, 100, free, now);
        assert!(t.open(end(990_000, 2), end(990_001, 2), now).is_none());
        assert_eq!(t.map.len(), MAX_SESSIONS);
    }
}
