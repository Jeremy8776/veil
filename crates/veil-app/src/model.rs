//! What the interface shows about a call: who is in it, who waits in the lobby, the chat,
//! and how each link is carried.
//!
//! Purpose: the screens draw only from [`Call`] and change it only through its methods, so the
//! real call engine ([`crate::engine`]) and the demo ([`crate::demo`]) sit behind it without
//! the screens changing.
//!
//! Invariants:
//! - Exactly one participant is you, and it is first.
//! - Only the host admits, declines or removes; the methods do nothing for a guest.
//! - In a real call those three only queue an [`Action`] for the engine. The people list
//!   changes when the engine reports the new key epoch, never before. A demo call changes at
//!   once, as MLS would.
//! - In a real call a chat message shows at once and is queued as [`Action::Chat`]; the
//!   engine seals it and sends it to everyone in the call.
//! - Verified is your own label for a device key you compared safety numbers with. It is
//!   kept between calls and never lets anyone in by itself.

use std::time::{Duration, Instant};

/// How a member's media reaches you, as `veil-probe` prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPath {
    Direct,
    Punched,
    Relayed,
    Forwarder,
    /// Keys are agreed but the media link is not up yet.
    Pending,
}

impl LinkPath {
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "Direct",
            Self::Punched => "Direct (hole punched)",
            Self::Relayed => "Relayed",
            Self::Forwarder => "Through forwarder",
            Self::Pending => "Connecting",
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
    /// You marked this device as verified, in this call or an earlier one.
    pub verified: bool,
}

#[derive(Clone, Debug)]
pub struct Waiting {
    pub id: u32,
    pub name: String,
    pub since: Instant,
    /// Safety number with this person, to compare before letting them in. Empty in a demo.
    pub safety: String,
    /// You verified this device in an earlier call. The host still decides.
    pub verified: bool,
}

/// Where a call stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallStatus {
    /// Setting up the network.
    Connecting,
    /// A guest waiting for the host to let them in.
    Lobby,
    Live,
    /// The call is over for you; the text says why.
    Ended(String),
}

/// For a host: whether the router opened the call's port (`engine/router.rs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RouterPort {
    /// Not asked for, or not hosting.
    #[default]
    Off,
    Opening,
    /// Guests on the internet can dial this address.
    Open(String),
    /// Not opened; the text is the guide for doing it by hand.
    Failed(String),
}

/// A host's decision, or a chat message, queued for the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Admit(u32),
    Decline(u32),
    Remove(u32),
    Chat(String),
    /// Mark a person's device as verified (`true`) or forget it (`false`).
    Verify(u32, bool),
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
    pub status: CallStatus,
    pub router: RouterPort,
    /// Decisions and chat not yet handed to the engine. Always empty in a demo.
    pub outbox: Vec<Action>,
    /// Chat messages not yet seen with the chat panel open.
    pub unread: usize,
    pub(crate) next_id: u32,
}

impl Call {
    /// A real call with only you in it, before the engine reports anything.
    pub fn live(title: String, your_name: &str, is_host: bool, now: Instant) -> Self {
        Self {
            title,
            is_host,
            started: now,
            participants: vec![Participant {
                id: 0,
                name: your_name.to_string(),
                is_you: true,
                is_host,
                muted: false,
                camera: false,
                hand_raised: false,
                speaking: false,
                path: LinkPath::Direct,
                safety: String::new(),
                verified: false,
            }],
            lobby: Vec::new(),
            chat: Vec::new(),
            invite: String::new(),
            epoch: 0,
            demo: false,
            status: CallStatus::Connecting,
            router: RouterPort::Off,
            outbox: Vec::new(),
            unread: 0,
            next_id: 1,
        }
    }

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
        if !self.demo {
            self.outbox.push(Action::Admit(id));
            return;
        }
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
            verified: waiting.verified,
        });
        self.epoch += 1;
    }

    pub fn decline(&mut self, id: u32) {
        if !self.is_host || !self.lobby.iter().any(|w| w.id == id) {
            return;
        }
        self.lobby.retain(|w| w.id != id);
        if !self.demo {
            self.outbox.push(Action::Decline(id));
        }
    }

    pub fn remove(&mut self, id: u32) {
        if !self.is_host {
            return;
        }
        if !self.demo {
            if self.participants.iter().any(|p| !p.is_you && p.id == id) {
                self.outbox.push(Action::Remove(id));
            }
            return;
        }
        let before = self.participants.len();
        self.participants.retain(|p| p.is_you || p.id != id);
        if self.participants.len() != before {
            self.epoch += 1;
        }
    }

    /// Marks someone in the call or the lobby as verified, or forgets them. The label shows
    /// at once; in a real call the engine saves it.
    pub fn set_verified(&mut self, id: u32, verified: bool) {
        let mut found = false;
        for p in self
            .participants
            .iter_mut()
            .filter(|p| !p.is_you && p.id == id)
        {
            p.verified = verified;
            found = true;
        }
        for w in self.lobby.iter_mut().filter(|w| w.id == id) {
            w.verified = verified;
            found = true;
        }
        if found && !self.demo {
            self.outbox.push(Action::Verify(id, verified));
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
        if !self.demo {
            self.outbox.push(Action::Chat(text.to_string()));
        }
    }

    /// A chat message from someone else, unread until the chat panel shows it.
    pub fn receive(&mut self, from: String, text: String, now: Instant) {
        self.chat.push(Message {
            from,
            mine: false,
            at: self.elapsed(now),
            text,
        });
        self.unread += 1;
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
            safety: String::new(),
            verified: false,
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
            safety: String::new(),
            verified: false,
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
            safety: String::new(),
            verified: false,
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
    fn a_live_call_queues_decisions_for_the_engine() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", true, now);
        call.lobby.push(Waiting {
            id: 7,
            name: "Guest".into(),
            since: now,
            safety: String::new(),
            verified: false,
        });
        call.admit(7);
        assert!(call.lobby.is_empty());
        assert_eq!(
            call.participants.len(),
            1,
            "joins only when the engine says so"
        );
        call.decline(8);
        call.remove(9);
        assert_eq!(call.outbox, [Action::Admit(7)]);
        call.participants.push(Participant {
            id: 9,
            is_you: false,
            ..call.participants[0].clone()
        });
        call.remove(9);
        assert_eq!(call.outbox, [Action::Admit(7), Action::Remove(9)]);
        assert_eq!(call.epoch, 0);
    }

    #[test]
    fn live_chat_is_queued_for_the_engine_and_received_chat_is_unread() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", false, now);
        call.send(" hi all ", now);
        assert_eq!(call.outbox, [Action::Chat("hi all".into())]);
        assert!(call.chat[0].mine);
        call.receive("Priya".into(), "hello".into(), now);
        assert_eq!(call.unread, 1);
        assert_eq!(call.chat[1].from, "Priya");
        assert!(!call.chat[1].mine);

        let mut demo = demo::call("Jo", true, now);
        demo.send("hi", now);
        assert!(demo.outbox.is_empty(), "a demo sends nothing");
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(Duration::from_secs(65)), "01:05");
        assert_eq!(clock(Duration::from_secs(3_725)), "1:02:05");
    }
}

#[cfg(test)]
mod verify_tests {
    use super::*;

    #[test]
    fn verifying_labels_at_once_and_queues_for_the_engine() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", true, now);
        call.lobby.push(Waiting {
            id: 4,
            name: "Guest".into(),
            since: now,
            safety: String::new(),
            verified: false,
        });
        call.set_verified(4, true);
        assert!(call.lobby[0].verified);
        call.set_verified(0, true);
        assert_eq!(call.outbox, [Action::Verify(4, true)], "not yourself");
    }
}
