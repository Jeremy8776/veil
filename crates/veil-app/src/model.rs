//! What the interface shows about a call: who is in it, who waits in the lobby, the chat,
//! and how each link is carried.
//!
//! Purpose: the screens draw only from [`Call`] and change it only through its methods, so the
//! demo ([`crate::demo`]) and, next, the real call engine (`veil-call`) can sit behind it
//! without the screens changing.
//!
//! Invariants:
//! - Exactly one participant is you, and it is first.
//! - Admitting or removing someone starts a new key epoch, as MLS does in `veil-group`.
//! - Only the host admits, declines or removes; the methods do nothing for a guest.

use std::time::{Duration, Instant};

/// How a member's media reaches you, as `veil-probe` prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPath {
    Direct,
    Punched,
    Relayed,
    Forwarder,
}

impl LinkPath {
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "Direct",
            Self::Punched => "Direct (hole punched)",
            Self::Relayed => "Relayed",
            Self::Forwarder => "Through forwarder",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Participant {
    pub id: u32,
    pub name: String,
    pub is_you: bool,
    pub is_host: bool,
    pub muted: bool,
    pub camera: bool,
    pub hand_raised: bool,
    pub speaking: bool,
    pub path: LinkPath,
    /// Safety number in groups of five digits.
    pub safety: String,
}

#[derive(Clone, Debug)]
pub struct Waiting {
    pub id: u32,
    pub name: String,
    pub since: Instant,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub from: String,
    pub mine: bool,
    /// Time into the call.
    pub at: Duration,
    pub text: String,
}

pub struct Call {
    pub title: String,
    pub is_host: bool,
    pub started: Instant,
    pub participants: Vec<Participant>,
    pub lobby: Vec<Waiting>,
    pub chat: Vec<Message>,
    pub invite: String,
    pub epoch: u64,
    /// Other participants are simulated.
    pub demo: bool,
    /// Chat messages not yet seen with the chat panel open.
    pub unread: usize,
    pub(crate) next_id: u32,
}

impl Call {
    pub fn you(&self) -> Option<&Participant> {
        self.participants.iter().find(|p| p.is_you)
    }

    pub fn you_mut(&mut self) -> Option<&mut Participant> {
        self.participants.iter_mut().find(|p| p.is_you)
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    pub fn admit(&mut self, id: u32) {
        if !self.is_host {
            return;
        }
        let Some(index) = self.lobby.iter().position(|w| w.id == id) else {
            return;
        };
        let waiting = self.lobby.remove(index);
        self.participants.push(Participant {
            id: waiting.id,
            safety: crate::demo::safety_number(waiting.id),
            name: waiting.name,
            is_you: false,
            is_host: false,
            muted: true,
            camera: false,
            hand_raised: false,
            speaking: false,
            path: LinkPath::Relayed,
        });
        self.epoch += 1;
    }

    pub fn decline(&mut self, id: u32) {
        if self.is_host {
            self.lobby.retain(|w| w.id != id);
        }
    }

    pub fn remove(&mut self, id: u32) {
        if !self.is_host {
            return;
        }
        let before = self.participants.len();
        self.participants.retain(|p| p.is_you || p.id != id);
        if self.participants.len() != before {
            self.epoch += 1;
        }
    }

    pub fn send(&mut self, text: &str, now: Instant) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let from = self.you().map(|p| p.name.clone()).unwrap_or_default();
        self.chat.push(Message {
            from,
            mine: true,
            at: self.elapsed(now),
            text: text.to_string(),
        });
    }

    /// Whoever should fill the stage in speaker view: the speaker, else the first other
    /// person, else you.
    pub fn focus(&self) -> Option<&Participant> {
        self.participants
            .iter()
            .find(|p| p.speaking && !p.is_you)
            .or_else(|| self.participants.iter().find(|p| !p.is_you))
            .or_else(|| self.participants.first())
    }
}

/// `mm:ss`, or `h:mm:ss` past an hour.
pub fn clock(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::demo;

    #[test]
    fn the_host_admits_declines_and_removes() {
        let now = Instant::now();
        let mut call = demo::call("Jo", true, now);
        call.lobby.push(Waiting {
            id: 99,
            name: "Priya Shah".into(),
            since: now,
        });
        let epoch = call.epoch;
        call.admit(99);
        assert!(call.participants.iter().any(|p| p.id == 99));
        assert_eq!(call.epoch, epoch + 1);
        call.remove(99);
        assert!(!call.participants.iter().any(|p| p.id == 99));
        assert_eq!(call.epoch, epoch + 2);
        call.lobby.push(Waiting {
            id: 100,
            name: "Sam".into(),
            since: now,
        });
        call.decline(100);
        assert!(call.lobby.is_empty());
    }

    #[test]
    fn a_guest_cannot_admit_or_remove() {
        let now = Instant::now();
        let mut call = demo::call("Jo", false, now);
        call.lobby.push(Waiting {
            id: 99,
            name: "Priya".into(),
            since: now,
        });
        call.admit(99);
        assert_eq!(call.lobby.len(), 1);
        let other = call.participants[1].id;
        call.remove(other);
        assert!(call.participants.iter().any(|p| p.id == other));
    }

    #[test]
    fn you_cannot_be_removed_and_empty_chat_is_ignored() {
        let now = Instant::now();
        let mut call = demo::call("Jo", true, now);
        let you = call.you().unwrap().id;
        call.remove(you);
        assert!(call.you().is_some());
        let before = call.chat.len();
        call.send("   ", now);
        assert_eq!(call.chat.len(), before);
        call.send(" hello ", now);
        assert_eq!(call.chat.last().unwrap().text, "hello");
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(Duration::from_secs(65)), "01:05");
        assert_eq!(clock(Duration::from_secs(3_725)), "1:02:05");
    }
}
