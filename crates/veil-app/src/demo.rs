//! A simulated call, behind the "Demo call" entry on Home, to try the interface alone.
//!
//! Purpose: fills a [`Call`] with people who take turns speaking, someone who knocks in the
//! lobby, and a chat message, on a fixed timeline. The interface shows a banner whenever
//! `Call::demo` is set, so nobody mistakes it for a real call.
//!
//! Invariants:
//! - Deterministic: the same elapsed time gives the same state, so it is testable.
//! - Safety numbers here are made up and labelled as demo values in the interface.
//!

use std::time::{Duration, Instant};

use crate::model::{Call, LinkPath, Message, Participant, Waiting};

/// (name, muted, camera on, link path)
const PEOPLE: [(&str, bool, bool, LinkPath); 4] = [
    ("Amara Okafor", false, false, LinkPath::Punched),
    ("Kenji Watanabe", false, false, LinkPath::Direct),
    ("Lucía Fernández", true, false, LinkPath::Relayed),
    ("Sam Patel", false, false, LinkPath::Forwarder),
];
/// When someone knocks, and when a chat message arrives.
const KNOCK_AT: Duration = Duration::from_secs(8);
const MESSAGE_AT: Duration = Duration::from_secs(14);
/// How long each person speaks in turn.
const TURN: Duration = Duration::from_millis(2_600);

/// A demo call with you first. As host you start it; as a guest someone else did.
pub fn call(your_name: &str, is_host: bool, now: Instant) -> Call {
    let mut participants = vec![Participant {
        id: 1,
        name: your_name.to_string(),
        is_you: true,
        is_host,
        muted: false,
        camera: true,
        hand_raised: false,
        speaking: false,
        path: LinkPath::Direct,
        safety: safety_number(1),
        verified: false,
    }];
    for (i, (name, muted, camera, path)) in PEOPLE.iter().enumerate() {
        let id = 2 + i as u32;
        participants.push(Participant {
            id,
            name: (*name).to_string(),
            is_you: false,
            is_host: !is_host && i == 0,
            muted: *muted,
            camera: *camera,
            hand_raised: false,
            speaking: false,
            path: *path,
            safety: safety_number(id),
            verified: false,
        });
    }
    Call {
        title: String::from("Weekly sync"),
        is_host,
        started: now,
        participants,
        lobby: Vec::new(),
        chat: Vec::new(),
        invite: String::from("veil://join/demo-invite-not-a-real-link"),
        invite_full: String::from("veil://join/demo-invite-not-a-real-link"),
        invite_expires: 0,
        invite_until_end: false,
        epoch: 1,
        demo: true,
        status: crate::model::CallStatus::Live,
        router: crate::model::RouterPort::Off,
        reach: crate::model::Reachable::Unknown,
        outbox: Vec::new(),
        unread: 0,
        next_id: 10,
    }
}

/// Moves the simulation to `now`: whose turn it is to speak, the knock, the message.
pub fn tick(call: &mut Call, now: Instant) {
    let elapsed = call.elapsed(now);
    let speakers: Vec<u32> = call
        .participants
        .iter()
        .filter(|p| !p.is_you && !p.muted)
        .map(|p| p.id)
        .collect();
    let turn = (elapsed.as_millis() / TURN.as_millis()) as usize;
    // Every fourth turn nobody else speaks, which leaves room for you.
    let current = (turn % 4 != 3 && !speakers.is_empty()).then(|| speakers[turn % speakers.len()]);
    for p in call.participants.iter_mut().filter(|p| !p.is_you) {
        p.speaking = Some(p.id) == current;
    }
    if call.is_host && elapsed >= KNOCK_AT && call.next_id == 10 {
        call.lobby.push(Waiting {
            id: call.next_id,
            name: String::from("Priya Shah"),
            since: now,
            safety: String::new(),
            verified: false,
        });
        call.next_id += 1;
    }
    if elapsed >= MESSAGE_AT && !call.chat.iter().any(|m| !m.mine) {
        call.chat.push(Message {
            from: PEOPLE[1].0.to_string(),
            mine: false,
            at: MESSAGE_AT,
            text: String::from("Can everyone see the safety numbers match on their side?"),
        });
        call.unread += 1;
    }
}

/// A made-up safety number in Veil's format: twelve groups of five digits.
pub fn safety_number(seed: u32) -> String {
    let mut x = u64::from(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..12)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            format!("{:05}", x % 100_000)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_take_turns_and_one_at_a_time() {
        let start = Instant::now();
        let mut call = call("Jo", true, start);
        let mut seen = std::collections::BTreeSet::new();
        for step in 0..12u64 {
            tick(&mut call, start + TURN * step as u32);
            let speaking: Vec<_> = call.participants.iter().filter(|p| p.speaking).collect();
            assert!(speaking.len() <= 1);
            seen.extend(speaking.iter().map(|p| p.id));
        }
        // Lucía is muted, so three others speak.
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn someone_knocks_once_and_a_message_arrives_once() {
        let start = Instant::now();
        let mut call = call("Jo", true, start);
        tick(&mut call, start + Duration::from_secs(7));
        assert!(call.lobby.is_empty());
        for s in [8, 9, 20] {
            tick(&mut call, start + Duration::from_secs(s));
        }
        assert_eq!(call.lobby.len(), 1);
        assert_eq!(call.chat.len(), 1);
        assert_eq!(call.unread, 1);
    }

    #[test]
    fn a_guest_sees_no_knocks() {
        let start = Instant::now();
        let mut call = call("Jo", false, start);
        tick(&mut call, start + Duration::from_secs(30));
        assert!(call.lobby.is_empty());
        assert!(call.participants[1].is_host);
    }

    #[test]
    fn safety_numbers_have_twelve_groups() {
        let number = safety_number(3);
        assert_eq!(number.split(' ').count(), 12);
        assert!(number.split(' ').all(|g| g.len() == 5));
        assert_ne!(number, safety_number(4));
    }
}
