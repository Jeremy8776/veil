//! Safety numbers: a 60-digit code two people compare to confirm they hold each other's real
//! public keys and nobody sits in the middle.
//!
//! Each key gives a 30-digit fingerprint from 5,200 rounds of SHA-512 (slow on purpose, so an
//! attacker cannot cheaply search for a key with a matching code). The two fingerprints are
//! sorted, so both people see the same number.
//!
//! Share codes let people compare without reading digits: each sends theirs through another
//! app and pastes the one they receive. A code is the whole number plus a side letter. The
//! code can sit inside a longer message ("Veil code from Jo for Sam ..."), or several codes
//! can be pasted at once: [`find_codes`] picks every code out of the text.
//!
//! Invariants:
//! - A check compares all 60 digits. Never shorten it: a short match can be searched for.
//! - The two people hold different side letters, because they have the same number. Without
//!   this, pasting your own copied code by mistake would look like a match.
//! - The letter only catches that mistake. It is not a secret, and neither is the code.
//! - Words around a code are never trusted. Only the letter and the digits are checked.

use std::fmt::Write as _;

use sha2::{Digest, Sha512};

use crate::PublicId;

pub(crate) const ITERATIONS: u32 = 5_200;
const DOMAIN: &[u8] = b"veil-safety-v1";
/// Digits in a safety number.
pub(crate) const DIGITS: usize = 60;
/// Longest pasted text that is searched for codes. Enough for a few messages from a chat.
pub const PASTE_LIMIT: usize = 4_000;

/// `bytes` as five-digit groups, five bytes per group, appended to `out` without spaces.
pub(crate) fn push_digits(bytes: &[u8], out: &mut String) {
    let (chunks, _) = bytes.as_chunks::<5>();
    for chunk in chunks {
        let value = chunk.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        // Writing to a String cannot fail.
        let _ = write!(out, "{:05}", value % 100_000);
    }
}

/// Digits in groups of five, separated by spaces.
pub(crate) fn grouped(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 5);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

pub(crate) fn fingerprint(key: &PublicId) -> String {
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
    push_digits(&hash[..30], &mut out);
    out
}

/// The safety number for a pair of keys, in 12 groups of 5 digits. Order does not matter.
pub fn safety_number(a: &PublicId, b: &PublicId) -> String {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    grouped(&(fingerprint(low) + &fingerprint(high)))
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

/// Each person's tag: the first five digits of their own fingerprint. It says whose code a
/// group code is. Returns (yours, theirs), read from the safety number you share and your
/// side of it: side A holds the first half.
pub fn tags(number: &str, mine: Side) -> Option<(String, String)> {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != DIGITS {
        return None;
    }
    let first = digits.get(..5)?.to_string();
    let second = digits.get(30..35)?.to_string();
    Some(match mine {
        Side::A => (first, second),
        Side::B => (second, first),
    })
}

/// The tag for one key, the same as [`tags`] gives for it.
pub fn tag(key: &PublicId) -> String {
    fingerprint(key).chars().take(5).collect()
}

/// The text one person sends the other: `VEIL-A 12345 67890 ...`.
pub fn share_code(number: &str, side: Side) -> String {
    format!("VEIL-{} {number}", side.letter())
}

/// Every code in `pasted`, as its letter and its first `len` digits. Spaces, hyphens and
/// letter case do not matter, since messaging apps can change them. Only the first
/// [`PASTE_LIMIT`] characters are read.
pub(crate) fn find_codes(pasted: &str, len: usize) -> Vec<(char, String)> {
    let text: Vec<char> = pasted
        .chars()
        .take(PASTE_LIMIT)
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(rest) = text.get(i..) {
        let Some(after) = rest.strip_prefix(&['V', 'E', 'I', 'L'][..]) else {
            i += 1;
            continue;
        };
        let Some((&letter, tail)) = after.split_first() else {
            break;
        };
        let digits: String = tail.iter().take_while(|c| c.is_ascii_digit()).take(len).collect();
        if digits.len() == len {
            found.push((letter, digits));
            i += 5 + len;
        } else {
            i += 1;
        }
    }
    found
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

/// Checks pasted text against `number`. The text may be a whole message, or several. The best
/// answer wins: their code, then your own (you copied the wrong message), then a different one.
pub fn check_code(number: &str, mine: Side, pasted: &str) -> CodeCheck {
    let ours: String = number.chars().filter(char::is_ascii_digit).collect();
    let mut best = CodeCheck::NotACode;
    for (letter, digits) in find_codes(pasted, DIGITS) {
        let side = match letter {
            'A' => Side::A,
            'B' => Side::B,
            _ => continue,
        };
        let check = if digits != ours {
            CodeCheck::Different
        } else if side == mine {
            CodeCheck::OwnCode
        } else {
            CodeCheck::Match
        };
        best = match (best, check) {
            (_, CodeCheck::Match) | (CodeCheck::Match, _) => CodeCheck::Match,
            (_, CodeCheck::OwnCode) | (CodeCheck::OwnCode, _) => CodeCheck::OwnCode,
            _ => CodeCheck::Different,
        };
    }
    best
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

    #[test]
    fn a_code_inside_a_message_matches() {
        let code = share_code(&number(), Side::B);
        let message =
            format!("Veil code from Sam for Jo. Paste it into Veil to check.\n{code}\nThanks");
        assert_eq!(check_code(&number(), Side::A, &message), CodeCheck::Match);
    }

    #[test]
    fn their_code_wins_among_several() {
        let other = safety_number(&PublicId([3; 32]), &PublicId([4; 32]));
        let pasted = format!(
            "{}\n{}\n{}",
            share_code(&other, Side::A),
            share_code(&number(), Side::A),
            share_code(&number(), Side::B)
        );
        assert_eq!(check_code(&number(), Side::A, &pasted), CodeCheck::Match);
        let without_theirs = format!(
            "{}\n{}",
            share_code(&other, Side::B),
            share_code(&number(), Side::A)
        );
        assert_eq!(check_code(&number(), Side::A, &without_theirs), CodeCheck::OwnCode);
    }

    #[test]
    fn a_number_on_the_next_line_does_not_spoil_a_code() {
        let pasted = format!("{}\n10:38", share_code(&number(), Side::B));
        assert_eq!(check_code(&number(), Side::A, &pasted), CodeCheck::Match);
    }

    #[test]
    fn tags_are_each_persons_own() {
        let a = PublicId([1; 32]);
        let b = PublicId([2; 32]);
        let n = safety_number(&a, &b);
        assert_eq!(tags(&n, side(&a, &b)), Some((tag(&a), tag(&b))));
        assert_eq!(tags(&n, side(&b, &a)), Some((tag(&b), tag(&a))));
        assert_eq!(tags("12345", Side::A), None);
    }
}
