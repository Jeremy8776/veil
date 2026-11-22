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
//!
//! shortcut: no replay protection. Someone who watches a real dial on the wire can copy its
//! knock into their own handshake and learn the listener's key. Add a time window and a seen
//! nonce cache if passive observers on the path become part of the threat model.

use sha2::{Digest, Sha256};
use veil_security::PublicId;

use crate::NetError;

const PREFIX: &[u8] = b"veil-knock/1:";
const DOMAIN: &[u8] = b"veil knock v1";
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 16;
const KNOCK_LEN: usize = PREFIX.len() + NONCE_LEN + TAG_LEN;

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

/// Whether `entry` (one ALPN entry from a ClientHello) is a knock for `listener`.
pub(crate) fn valid(listener: &PublicId, entry: &[u8]) -> bool {
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
        assert!(!valid(&PublicId([7; 32]), &make(&PublicId([8; 32])).unwrap()));
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
}
