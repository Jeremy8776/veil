//! Group codes: one code per person that checks them with everyone in the call.
//!
//! Purpose: with pair codes, five people need ten swaps. With a group code, each person posts
//! one message in a group chat, and everyone pastes what was posted.
//!
//! The group number is 60 digits from every device key in the call, as this device sees them,
//! after 5,200 rounds of SHA-512 like a safety number. A code is `VEIL-G`, then the sender's
//! tag (the first five digits of their fingerprint, see `safety::tag`), then the key epoch's
//! last five digits, then the number.
//!
//! What a match proves: the sender's set of keys and yours are the same. Your real key is in
//! your set, so the sender holds it; their real key is in theirs, so you hold it. That is the
//! same as swapping pair codes with that one person. It says nothing about anyone who has not
//! posted a code, because two people can be shown the same fake key for a third.
//!
//! Invariants:
//! - A match verifies the sender only, found by tag. Never everyone in the call.
//! - All 60 digits are compared. The tag and epoch only route and explain; a wrong tag or a
//!   ground-out matching tag still needs the 60 digits to match.
//! - Your own tag never matches, so pasting your own code back verifies nobody.
//! - A code from another epoch never matches. It is reported as old, not as an attack,
//!   because someone joining or leaving changes the number.

use sha2::{Digest, Sha512};

use crate::PublicId;
use crate::safety::{DIGITS, ITERATIONS, find_codes, grouped, push_digits};

const DOMAIN: &[u8] = b"veil-group-v1";
/// Digits in a tag, and in the epoch part of a code.
const TAG: usize = 5;
const EPOCH: usize = 5;

/// The group number for everyone in a call, in 12 groups of 5 digits. Order and repeats do
/// not matter.
pub fn group_number(members: &[PublicId]) -> String {
    let mut keys: Vec<&PublicId> = members.iter().collect();
    keys.sort();
    keys.dedup();
    let count = u64::try_from(keys.len()).unwrap_or(u64::MAX);
    let mut first = Sha512::new()
        .chain_update(DOMAIN)
        .chain_update(count.to_be_bytes());
    for key in &keys {
        first.update(key.0);
    }
    let mut hash = first.finalize();
    for _ in 0..ITERATIONS {
        let mut next = Sha512::new().chain_update(hash);
        for key in &keys {
            next.update(key.0);
        }
        hash = next.finalize();
    }
    let mut digits = String::with_capacity(DIGITS);
    push_digits(&hash[..DIGITS], &mut digits);
    grouped(&digits)
}

fn epoch_digits(epoch: u64) -> String {
    format!("{:05}", epoch % 100_000)
}

/// The code one person posts: `VEIL-G <tag> <epoch> <number>`.
pub fn group_code(number: &str, tag: &str, epoch: u64) -> String {
    format!("VEIL-G {tag} {} {number}", epoch_digits(epoch))
}

/// What one code in the pasted text turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupCheck {
    /// The person with this tag sees the same keys as you.
    Match(String),
    /// Your own code.
    OwnCode,
    /// From before someone joined or left.
    Old,
    /// Same epoch, different keys: from someone who sees different keys from you.
    Different(String),
}

/// Checks every group code in `pasted` against your `number`, `tag` and `epoch`, in the order
/// they appear. Empty when the text holds no group code.
pub fn check_group(number: &str, tag: &str, epoch: u64, pasted: &str) -> Vec<GroupCheck> {
    let ours: String = number.chars().filter(char::is_ascii_digit).collect();
    let epoch = epoch_digits(epoch);
    find_codes(pasted, TAG + EPOCH + DIGITS)
        .into_iter()
        .filter(|(letter, _)| *letter == 'G')
        .map(|(_, digits)| {
            let (sender, rest) = digits.split_at(TAG);
            let (their_epoch, theirs) = rest.split_at(EPOCH);
            if sender == tag {
                GroupCheck::OwnCode
            } else if their_epoch != epoch {
                GroupCheck::Old
            } else if theirs == ours {
                GroupCheck::Match(sender.to_string())
            } else {
                GroupCheck::Different(sender.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::{CodeCheck, Side, check_code, share_code, tag as tag_of};

    fn keys() -> Vec<PublicId> {
        vec![PublicId([1; 32]), PublicId([2; 32]), PublicId([3; 32])]
    }

    #[test]
    fn order_and_repeats_do_not_matter() {
        let k = keys();
        let shuffled = [k[2], k[0], k[1], k[0]];
        assert_eq!(group_number(&k), group_number(&shuffled));
    }

    #[test]
    fn sixty_digits_in_twelve_groups() {
        let n = group_number(&keys());
        assert_eq!(n.split(' ').count(), 12);
        assert_eq!(n.chars().filter(char::is_ascii_digit).count(), 60);
    }

    #[test]
    fn a_swapped_key_changes_the_number() {
        let mut fake = keys();
        fake[2] = PublicId([9; 32]);
        assert_ne!(group_number(&keys()), group_number(&fake));
        assert_ne!(group_number(&keys()), group_number(&keys()[..2]));
    }

    #[test]
    fn someone_elses_code_matches_and_names_them() {
        let n = group_number(&keys());
        let theirs = tag_of(&keys()[1]);
        let message = format!("Veil group code from Sam.\n{}", group_code(&n, &theirs, 7));
        let checks = check_group(&n, &tag_of(&keys()[0]), 7, &message);
        assert_eq!(checks, vec![GroupCheck::Match(theirs)]);
    }

    #[test]
    fn your_own_code_verifies_nobody() {
        let n = group_number(&keys());
        let mine = tag_of(&keys()[0]);
        let checks = check_group(&n, &mine, 7, &group_code(&n, &mine, 7));
        assert_eq!(checks, vec![GroupCheck::OwnCode]);
    }

    #[test]
    fn an_old_code_is_old_not_a_match() {
        let n = group_number(&keys());
        let theirs = tag_of(&keys()[1]);
        let checks = check_group(&n, &tag_of(&keys()[0]), 8, &group_code(&n, &theirs, 7));
        assert_eq!(checks, vec![GroupCheck::Old]);
    }

    #[test]
    fn different_keys_are_different() {
        let mut fake = keys();
        fake[2] = PublicId([9; 32]);
        let theirs = tag_of(&keys()[1]);
        let posted = group_code(&group_number(&fake), &theirs, 7);
        let checks = check_group(&group_number(&keys()), &tag_of(&keys()[0]), 7, &posted);
        assert_eq!(checks, vec![GroupCheck::Different(theirs)]);
    }

    #[test]
    fn a_whole_chat_paste_checks_each_code() {
        let n = group_number(&keys());
        let [a, b, c] = [0, 1, 2].map(|i| tag_of(&keys()[i]));
        let chat = format!(
            "Jo 10:37\n{}\nSam 10:38\n{}\nAl 10:38\n{}",
            group_code(&n, &a, 7),
            group_code(&n, &b, 7),
            group_code(&n, &c, 7)
        );
        let checks = check_group(&n, &a, 7, &chat);
        assert_eq!(
            checks,
            vec![GroupCheck::OwnCode, GroupCheck::Match(b), GroupCheck::Match(c)]
        );
    }

    #[test]
    fn group_and_pair_codes_do_not_mix() {
        let n = group_number(&keys());
        let pair = share_code(&n, Side::B);
        assert!(check_group(&n, "00000", 7, &pair).is_empty());
        let group = group_code(&n, "12345", 7);
        assert_eq!(check_code(&n, Side::A, &group), CodeCheck::NotACode);
    }
}
