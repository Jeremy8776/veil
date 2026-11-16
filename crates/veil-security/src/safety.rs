//! Safety numbers: a 60-digit code two people compare to confirm they hold each other's real
//! public keys and nobody sits in the middle.
//!
//! Each key gives a 30-digit fingerprint from 5,200 rounds of SHA-512 (slow on purpose, so an
//! attacker cannot cheaply search for a key with a matching code). The two fingerprints are
//! sorted, so both people see the same number.

use std::fmt::Write as _;

use sha2::{Digest, Sha512};

use crate::PublicId;

const ITERATIONS: u32 = 5_200;
const DOMAIN: &[u8] = b"veil-safety-v1";

fn fingerprint(key: &PublicId) -> String {
    let mut hash = Sha512::new()
        .chain_update(DOMAIN)
        .chain_update(key.0)
        .finalize();
    for _ in 0..ITERATIONS {
        hash = Sha512::new()
            .chain_update(hash)
            .chain_update(key.0)
            .finalize();
    }
    let mut out = String::with_capacity(30);
    let (chunks, _) = hash[..30].as_chunks::<5>();
    for chunk in chunks {
        let value = chunk.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        // Writing to a String cannot fail.
        let _ = write!(out, "{:05}", value % 100_000);
    }
    out
}

/// The safety number for a pair of keys, in 12 groups of 5 digits. Order does not matter.
pub fn safety_number(a: &PublicId, b: &PublicId) -> String {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let digits = fingerprint(low) + &fingerprint(high);
    let mut out = String::with_capacity(71);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_for_both_people() {
        let a = PublicId([1; 32]);
        let b = PublicId([2; 32]);
        assert_eq!(safety_number(&a, &b), safety_number(&b, &a));
    }

    #[test]
    fn sixty_digits_in_twelve_groups() {
        let n = safety_number(&PublicId([1; 32]), &PublicId([2; 32]));
        assert_eq!(n.split(' ').count(), 12);
        assert_eq!(n.chars().filter(char::is_ascii_digit).count(), 60);
    }

    #[test]
    fn different_keys_give_different_numbers() {
        let a = PublicId([1; 32]);
        assert_ne!(
            safety_number(&a, &PublicId([2; 32])),
            safety_number(&a, &PublicId([3; 32]))
        );
    }
}
