//! Safety numbers: a 60-digit code two people compare to confirm they hold each other's real
//! public keys and nobody sits in the middle.
//!
//! Each key gives a 30-digit fingerprint from 5,200 rounds of SHA-512 (slow on purpose, so an
//! attacker cannot cheaply search for a key with a matching code). The two fingerprints are
//! sorted, so both people see the same number.
//!
//! Share codes let people compare without reading digits: each sends theirs through another
//! app and pastes the one they receive. A code is the whole number plus a side letter.
//!
//! Invariants:
//! - A check compares all 60 digits. Never shorten it: a short match can be searched for.
//! - The two people hold different side letters, because they have the same number. Without
//!   this, pasting your own copied code by mistake would look like a match.
//! - The letter only catches that mistake. It is not a secret, and neither is the code.

use std::fmt::Write as _;

use sha2::{Digest, Sha512};

use crate::PublicId;

const ITERATIONS: u32 = 5_200;
const DOMAIN: &[u8] = b"veil-safety-v1";
/// Digits in a safety number.
const DIGITS: usize = 60;

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

/// Which of the two people you are for a safety number. Each person gets the opposite side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    fn letter(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
        }
    }
}

/// Your side of the safety number you share with `them`.
pub fn side(me: &PublicId, them: &PublicId) -> Side {
    if me <= them { Side::A } else { Side::B }
}

/// The text one person sends the other: `VEIL-A 12345 67890 ...`.
pub fn share_code(number: &str, side: Side) -> String {
    format!("VEIL-{} {number}", side.letter())
}

/// What a pasted share code turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeCheck {
    /// The other person's code for this number.
    Match,
    /// Your own code, pasted back.
    OwnCode,
    /// A real code, but for a different number.
    Different,
    /// Not a share code at all.
    NotACode,
}

/// Checks a pasted code against `number`. Spaces, hyphens and letter case do not matter, since
/// messaging apps can change them.
pub fn check_code(number: &str, mine: Side, pasted: &str) -> CodeCheck {
    let text: String = pasted
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let Some(rest) = text.strip_prefix("VEIL") else {
        return CodeCheck::NotACode;
    };
    let mut chars = rest.chars();
    let side = match chars.next() {
        Some('A') => Side::A,
        Some('B') => Side::B,
        _ => return CodeCheck::NotACode,
    };
    let digits = chars.as_str();
    if digits.len() != DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return CodeCheck::NotACode;
    }
    if !digits.chars().eq(number.chars().filter(char::is_ascii_digit)) {
        CodeCheck::Different
    } else if side == mine {
        CodeCheck::OwnCode
    } else {
        CodeCheck::Match
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number() -> String {
        safety_number(&PublicId([1; 32]), &PublicId([2; 32]))
    }

    #[test]
    fn the_two_people_are_on_opposite_sides() {
        let a = PublicId([1; 32]);
        let b = PublicId([2; 32]);
        assert_eq!(side(&a, &b).other(), side(&b, &a));
    }

    #[test]
    fn the_other_persons_code_matches() {
        let code = share_code(&number(), Side::B);
        assert_eq!(check_code(&number(), Side::A, &code), CodeCheck::Match);
    }

    #[test]
    fn your_own_code_is_not_a_match() {
        let code = share_code(&number(), Side::A);
        assert_eq!(check_code(&number(), Side::A, &code), CodeCheck::OwnCode);
    }

    #[test]
    fn a_messaging_app_can_reformat_a_code() {
        let code = share_code(&number(), Side::B).to_lowercase().replace(' ', "\n");
        assert_eq!(check_code(&number(), Side::A, &code), CodeCheck::Match);
    }

    #[test]
    fn one_wrong_digit_is_different() {
        let mut code = share_code(&number(), Side::B);
        let last = if code.ends_with('0') { '1' } else { '0' };
        code.pop();
        code.push(last);
        assert_eq!(check_code(&number(), Side::A, &code), CodeCheck::Different);
    }

    #[test]
    fn partial_or_bare_numbers_are_refused() {
        let n = number();
        let short = share_code(&n[..11], Side::B);
        for pasted in ["", "hello", n.as_str(), short.as_str()] {
            assert_eq!(check_code(&n, Side::A, pasted), CodeCheck::NotACode);
        }
    }

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
