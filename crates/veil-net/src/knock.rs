//! Knock: a dialler proves it already knows a node's device key before the node shows it.
//!
//! Purpose: the device key is the same in every call, so a node that hands it to anyone who
//! starts a handshake lets an internet scanner recognise a Veil host and follow one device
//! across calls and addresses. Every real dialler already knows the key it is calling (an
//! invite, a roster, a beacon or a rendezvous names it), so the listener asks for proof of
//! that first and refuses everyone else before sending its key.
//!
//! The proof rides in the TLS ClientHello as a second ALPN entry:
//! `veil-knock/1:` + nonce (16 random bytes) + tag (16 bytes), where tag is the first 16 bytes
//! of SHA-256(DOMAIN || listener key || nonce). The listener still selects [`crate::tls::ALPN`].
//!
//! Invariants:
//! - A fresh nonce per dial, so a passive observer sees unlinkable bytes, not a key hash.
//! - Checking a knock needs only the listener's own public key; nothing is stored.
//! - Only nodes marked open (rendezvous servers, which serve short-invite fetches from
//!   diallers that cannot know their key) answer without a knock.
//! - A knock opens the door once per process. Someone who copies a knock off the wire gets
//!   refused, because the real dial already used it. The dialler makes a new knock for every
//!   address it tries, so happy eyeballs never sends one knock twice.
//! - Only valid knocks are remembered, and making one needs the key, so a stranger cannot
//!   flood the record to push real knocks out of it.
//!
//! The TLS handshake that follows is the signed challenge: each side signs a transcript that
//! holds both sides' fresh random values, so a replayed handshake never authenticates. The
//! knock only decides whether the listener shows its key at all.
//!
//! A HelloRetryRequest resends the ClientHello, and with it the knock, which this check then
//! refuses. Both sides use the same ring key-exchange groups, so none is sent today. Any
//! change to the groups (PQ-01) must keep this test passing:
//! `tls::tests::knocked_handshake_completes_and_its_copy_is_refused`.
//!
//! shortcut: the record lives in memory and holds the last `REMEMBERED` knocks, so a knock
//! copied before a restart, or more than `REMEMBERED` valid dials ago, still works once. Add a
//! signed time in the knock if a patient on-path observer becomes part of the threat model.

use std::collections::{HashSet, VecDeque};
use std::sync::{LazyLock, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use veil_security::PublicId;

use crate::NetError;

const PREFIX: &[u8] = b"veil-knock/1:";
const DOMAIN: &[u8] = b"veil knock v1";
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 16;
const KNOCK_LEN: usize = PREFIX.len() + NONCE_LEN + TAG_LEN;
/// How many used knocks the process remembers: 4096 x 16 bytes, about 64 KB.
const REMEMBERED: usize = 4096;

/// Knock nonces already used in this process, oldest first. Shared by every endpoint, so a
/// knock seen on a direct path cannot be replayed through a relayed one.
static SEEN: LazyLock<Mutex<Seen>> = LazyLock::new(|| Mutex::new(Seen::default()));

#[derive(Default)]
struct Seen {
    order: VecDeque<[u8; NONCE_LEN]>,
    set: HashSet<[u8; NONCE_LEN]>,
}

impl Seen {
    /// Records `nonce`. False when it was already recorded.
    fn first_use(&mut self, nonce: [u8; NONCE_LEN]) -> bool {
        if !self.set.insert(nonce) {
            return false;
        }
        self.order.push_back(nonce);
        if self.order.len() > REMEMBERED
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}

fn tag(listener: &PublicId, nonce: &[u8]) -> [u8; TAG_LEN] {
    let digest = Sha256::new()
        .chain_update(DOMAIN)
        .chain_update(listener.0)
        .chain_update(nonce)
        .finalize();
    let mut out = [0u8; TAG_LEN];
    out.copy_from_slice(&digest[..TAG_LEN]);
    out
}

/// A fresh knock for dialling the node whose device key is `listener`.
pub(crate) fn make(listener: &PublicId) -> Result<Vec<u8>, NetError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| NetError::Config(format!("no randomness: {e}")))?;
    let mut knock = Vec::with_capacity(KNOCK_LEN);
    knock.extend_from_slice(PREFIX);
    knock.extend_from_slice(&nonce);
    knock.extend_from_slice(&tag(listener, &nonce));
    Ok(knock)
}

/// Whether `entry` (one ALPN entry from a ClientHello) is a knock for `listener` that has not
/// been used before. A valid knock is used up by this call.
pub(crate) fn admit(listener: &PublicId, entry: &[u8]) -> bool {
    if !valid(listener, entry) {
        return false;
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&entry[PREFIX.len()..PREFIX.len() + NONCE_LEN]);
    // A poisoned lock only means another thread panicked between two inserts; the record is
    // still a set of used nonces.
    SEEN.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .first_use(nonce)
}

/// Whether `entry` is a well-formed knock for `listener`, used or not.
fn valid(listener: &PublicId, entry: &[u8]) -> bool {
    if entry.len() != KNOCK_LEN {
        return false;
    }
    let Some(rest) = entry.strip_prefix(PREFIX) else {
        return false;
    };
    let (nonce, given) = rest.split_at(NONCE_LEN);
    let expected = tag(listener, nonce);
    // Constant time, so timing says nothing about how much of the tag matched.
    given
        .iter()
        .zip(expected.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn knock_for_the_right_key_is_accepted() {
        let key = PublicId([7; 32]);
        assert!(valid(&key, &make(&key).unwrap()));
    }

    #[test]
    fn knock_for_another_key_is_refused() {
        assert!(!valid(
            &PublicId([7; 32]),
            &make(&PublicId([8; 32])).unwrap()
        ));
    }

    #[test]
    fn every_knock_is_different() {
        let key = PublicId([7; 32]);
        assert_ne!(make(&key).unwrap(), make(&key).unwrap());
    }

    #[test]
    fn changed_short_or_plain_entries_are_refused() {
        let key = PublicId([7; 32]);
        let mut knock = make(&key).unwrap();
        *knock.last_mut().unwrap() ^= 1;
        assert!(!valid(&key, &knock));
        assert!(!valid(&key, &knock[..KNOCK_LEN - 1]));
        assert!(!valid(&key, crate::tls::ALPN));
        assert!(!valid(&key, b""));
    }

    #[test]
    fn a_knock_opens_the_door_once() {
        let key = PublicId([9; 32]);
        let knock = make(&key).unwrap();
        assert!(admit(&key, &knock));
        assert!(!admit(&key, &knock), "a copied knock is refused");
        assert!(admit(&key, &make(&key).unwrap()), "a new knock still works");
    }

    #[test]
    fn an_invalid_knock_is_not_remembered() {
        let key = PublicId([10; 32]);
        let knock = make(&key).unwrap();
        assert!(!admit(&PublicId([11; 32]), &knock));
        assert!(
            admit(&key, &knock),
            "a failed check must not use the knock up"
        );
    }

    #[test]
    fn the_record_forgets_the_oldest_knock_when_full() {
        let nonce = |i: usize| {
            let mut n = [0u8; NONCE_LEN];
            n[..8].copy_from_slice(&(i as u64).to_le_bytes());
            n
        };
        let mut seen = Seen::default();
        for i in 0..=REMEMBERED {
            assert!(seen.first_use(nonce(i)));
        }
        assert_eq!(seen.order.len(), REMEMBERED);
        assert_eq!(seen.set.len(), REMEMBERED);
        assert!(seen.first_use(nonce(0)), "the oldest was forgotten");
        assert!(
            !seen.first_use(nonce(REMEMBERED)),
            "the newest is remembered"
        );
    }
}
