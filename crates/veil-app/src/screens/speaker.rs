//! Who fills the stage in Focus view, with a short hold so the stage does not flicker.
//!
//! Invariants:
//! - Your own tile takes focus only when nobody else is in the call.
//! - Someone who takes focus keeps it for at least [`HOLD`], even if another person
//!   starts speaking. After that, whoever is speaking takes over at once.
//! - When nobody is speaking, focus stays on the last speaker.
//! - When the focused person leaves, focus moves straight away.
use crate::model::Participant;
use std::time::{Duration, Instant};

/// The shortest time one person keeps the stage.
pub const HOLD: Duration = Duration::from_secs(2);

/// The person on stage and when they took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    pub id: u32,
    pub since: Instant,
}

/// What the choice needs to know about one person.
#[derive(Clone, Copy)]
pub struct Seat {
    pub id: u32,
    pub speaking: bool,
    pub is_you: bool,
}

impl From<&Participant> for Seat {
    fn from(p: &Participant) -> Self {
        Self {
            id: p.id,
            speaking: p.speaking,
            is_you: p.is_you,
        }
    }
}

/// The next focus, given the last one. Pure, so the hold is testable without a clock.
pub fn pick(last: Option<Focus>, people: &[Seat], now: Instant) -> Option<Focus> {
    let others: Vec<_> = people.iter().filter(|p| !p.is_you).collect();
    let take = |id| Some(Focus { id, since: now });
    if others.is_empty() {
        let you = people.first()?.id;
        return match last {
            Some(f) if f.id == you => last,
            _ => take(you),
        };
    }
    let current = last.and_then(|f| others.iter().find(|p| p.id == f.id).map(|p| (f, p)));
    let speaker = others.iter().find(|p| p.speaking).map(|p| p.id);
    match (current, speaker) {
        (Some((f, p)), _) if p.speaking => Some(f),
        (Some((f, _)), None) => Some(f),
        (Some((f, _)), Some(_)) if now.saturating_duration_since(f.since) < HOLD => Some(f),
        (_, Some(id)) => take(id),
        (None, None) => others.first().and_then(|p| take(p.id)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn seat(id: u32, speaking: bool) -> Seat {
        Seat {
            id,
            speaking,
            is_you: id == 0,
        }
    }

    #[test]
    fn you_never_take_focus_unless_alone() {
        let now = Instant::now();
        let f = pick(None, &[seat(0, true), seat(1, false)], now).unwrap();
        assert_eq!(f.id, 1);
        let alone = pick(Some(f), &[seat(0, true)], now).unwrap();
        assert_eq!(alone.id, 0);
    }

    #[test]
    fn a_new_speaker_waits_for_the_hold() {
        let t0 = Instant::now();
        let f = pick(None, &[seat(1, true), seat(2, false)], t0).unwrap();
        assert_eq!(f.id, 1);
        let early = t0 + HOLD / 2;
        let held = pick(Some(f), &[seat(1, false), seat(2, true)], early).unwrap();
        assert_eq!(held, f, "no switch inside the hold");
        let late = t0 + HOLD;
        let moved = pick(Some(held), &[seat(1, false), seat(2, true)], late).unwrap();
        assert_eq!(moved, Focus { id: 2, since: late });
    }

    #[test]
    fn the_current_speaker_keeps_focus_while_speaking() {
        let t0 = Instant::now();
        let f = Focus { id: 1, since: t0 };
        let later = t0 + HOLD * 5;
        let kept = pick(Some(f), &[seat(1, true), seat(2, true)], later).unwrap();
        assert_eq!(kept, f);
    }

    #[test]
    fn silence_keeps_the_last_speaker() {
        let t0 = Instant::now();
        let f = Focus { id: 2, since: t0 };
        let kept = pick(Some(f), &[seat(1, false), seat(2, false)], t0 + HOLD * 3).unwrap();
        assert_eq!(kept, f);
    }

    #[test]
    fn focus_moves_at_once_when_the_person_leaves() {
        let t0 = Instant::now();
        let f = Focus { id: 2, since: t0 };
        let moved = pick(Some(f), &[seat(0, false), seat(1, false)], t0).unwrap();
        assert_eq!(moved.id, 1);
        assert!(pick(None, &[], t0).is_none());
    }
}
