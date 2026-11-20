//! The call's chat as the interface shows it: messages and the reactions on them.
//!
//! Purpose: keeps chat and reactions apart from the rest of [`Call`] (`model.rs`), which
//! owns everything else the screens draw.
//!
//! Invariants:
//! - In a real call your message and your reaction show at once and are queued as
//!   [`Action::Chat`] and [`Action::React`]; the engine seals them and sends them to everyone.
//! - A message is named by its id. The first message with an id wins; a later one with the
//!   same id is dropped, so a member cannot take over another's message by copying its id.
//! - Messages from 0.1.11 and earlier have no id and take no reactions.
//! - Each person counts once per emoji on a message, and a message holds at most
//!   [`MAX_EMOJI_PER_MESSAGE`] different emoji, so one member cannot flood it.

use std::time::{Duration, Instant};

use veil_security::PublicId;

use crate::model::{Action, Call};

/// The most different emoji one message shows.
pub const MAX_EMOJI_PER_MESSAGE: usize = 12;

/// Offered when reacting to a message.
pub const QUICK_REACTIONS: [&str; 6] = ["👍", "❤", "😂", "😮", "😢", "🎉"];

/// Offered by the emoji button beside the message box. Single characters only: egui draws a
/// character at a time, so a joined emoji (skin tones, families) would show in pieces.
pub const PICKER: [&str; 40] = [
    "😀", "😃", "😄", "😁", "😆", "😅", "😂", "🙂", "😉", "😊", "😍", "😘", "😎", "🤔", "😐",
    "😮", "😢", "😭", "😡", "🙄", "😴", "🤯", "🥳", "🤝", "👍", "👎", "👏", "🙌", "🙏", "👋",
    "💪", "👀", "❤", "🔥", "🎉", "✨", "💯", "✅", "❌", "☕",
];

/// Who reacted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reactor {
    You,
    /// Someone else, by device key: names can change, keys cannot.
    Device(PublicId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    /// Who chose this emoji, with the name each had when they chose it.
    pub by: Vec<(Reactor, String)>,
}

impl Reaction {
    pub fn includes_you(&self) -> bool {
        self.by.iter().any(|(who, _)| *who == Reactor::You)
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    /// None for a message from 0.1.11 and earlier, which takes no reactions.
    pub id: Option<u64>,
    pub from: String,
    pub mine: bool,
    /// Time into the call.
    pub at: Duration,
    pub text: String,
    /// In the order each emoji was first chosen.
    pub reactions: Vec<Reaction>,
}

impl Call {
    pub fn send(&mut self, text: &str, now: Instant) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let id = veil_call::app_msg::new_message_id();
        let from = self.you().map(|p| p.name.clone()).unwrap_or_default();
        self.chat.push(Message {
            id: Some(id),
            from,
            mine: true,
            at: self.elapsed(now),
            text: text.to_string(),
            reactions: Vec::new(),
        });
        if !self.demo {
            self.outbox.push(Action::Chat(id, text.to_string()));
        }
    }

    /// A chat message from someone else, unread until the chat panel shows it. Dropped if a
    /// message with the same id is already shown.
    pub fn receive(&mut self, from: String, id: Option<u64>, text: String, now: Instant) {
        if id.is_some() && self.chat.iter().any(|m| m.id == id) {
            return;
        }
        self.chat.push(Message {
            id,
            from,
            mine: false,
            at: self.elapsed(now),
            text,
            reactions: Vec::new(),
        });
        self.unread += 1;
    }

    /// Adds your reaction to message `id`, or takes it back if you already chose `emoji`.
    pub fn react(&mut self, id: u64, emoji: &str) {
        let you = self.you().map(|p| p.name.clone()).unwrap_or_default();
        let Some(message) = self.chat.iter_mut().find(|m| m.id == Some(id)) else {
            return;
        };
        let on = !message
            .reactions
            .iter()
            .any(|r| r.emoji == emoji && r.includes_you());
        if !set_reaction(message, Reactor::You, you, emoji, on) {
            return;
        }
        if !self.demo {
            self.outbox.push(Action::React(id, emoji.to_string(), on));
        }
    }

    /// Someone else's reaction to message `id` arrived. A reaction to a message not shown
    /// is dropped.
    // shortcut: a reaction that overtakes its message on another link is lost. Upgrade by
    // holding unmatched reactions briefly if people report missing ones.
    pub fn reacted(&mut self, by: PublicId, name: String, id: u64, emoji: &str, on: bool) {
        if let Some(message) = self.chat.iter_mut().find(|m| m.id == Some(id)) {
            set_reaction(message, Reactor::Device(by), name, emoji, on);
        }
    }
}

/// Adds or takes back one person's reaction. Returns whether anything changed.
fn set_reaction(message: &mut Message, who: Reactor, name: String, emoji: &str, on: bool) -> bool {
    let index = message.reactions.iter().position(|r| r.emoji == emoji);
    if !on {
        let Some(index) = index else { return false };
        let reaction = &mut message.reactions[index];
        let before = reaction.by.len();
        reaction.by.retain(|(w, _)| *w != who);
        let changed = reaction.by.len() != before;
        if reaction.by.is_empty() {
            message.reactions.remove(index);
        }
        return changed;
    }
    match index {
        Some(index) => {
            let by = &mut message.reactions[index].by;
            if by.iter().any(|(w, _)| *w == who) {
                return false;
            }
            by.push((who, name));
        }
        None if message.reactions.len() < MAX_EMOJI_PER_MESSAGE => {
            message.reactions.push(Reaction {
                emoji: emoji.to_string(),
                by: vec![(who, name)],
            });
        }
        None => return false,
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::demo;

    const PRIYA: PublicId = PublicId([1; 32]);
    const SAM: PublicId = PublicId([2; 32]);

    fn live_with_message(now: Instant) -> (Call, u64) {
        let mut call = Call::live("Call".into(), "Jo", false, now);
        call.receive("Priya".into(), Some(7), "hello".into(), now);
        (call, 7)
    }

    #[test]
    fn empty_chat_is_ignored_and_text_is_trimmed() {
        let now = Instant::now();
        let mut call = demo::call("Jo", true, now);
        let before = call.chat.len();
        call.send("   ", now);
        assert_eq!(call.chat.len(), before);
        call.send(" hello ", now);
        assert_eq!(call.chat.last().unwrap().text, "hello");
    }

    #[test]
    fn live_chat_is_queued_for_the_engine_and_received_chat_is_unread() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", false, now);
        call.send(" hi all ", now);
        let id = call.chat[0].id.unwrap();
        assert_eq!(call.outbox, [Action::Chat(id, "hi all".into())]);
        assert!(call.chat[0].mine);
        call.receive("Priya".into(), Some(9), "hello".into(), now);
        assert_eq!(call.unread, 1);
        assert_eq!(call.chat[1].from, "Priya");
        assert!(!call.chat[1].mine);

        let mut demo = demo::call("Jo", true, now);
        demo.send("hi", now);
        assert!(demo.outbox.is_empty(), "a demo sends nothing");
    }

    #[test]
    fn the_first_message_with_an_id_wins() {
        let now = Instant::now();
        let (mut call, id) = live_with_message(now);
        call.receive("Sam".into(), Some(id), "I said hello".into(), now);
        assert_eq!(call.chat.len(), 1);
        assert_eq!(call.chat[0].from, "Priya");
        // Messages from older apps have no id and never clash.
        call.receive("Old".into(), None, "a".into(), now);
        call.receive("Old".into(), None, "a".into(), now);
        assert_eq!(call.chat.len(), 3);
    }

    #[test]
    fn your_reaction_toggles_and_is_queued() {
        let now = Instant::now();
        let (mut call, id) = live_with_message(now);
        call.react(id, "👍");
        assert!(call.chat[0].reactions[0].includes_you());
        call.react(id, "👍");
        assert!(call.chat[0].reactions.is_empty());
        assert_eq!(
            call.outbox,
            [
                Action::React(id, "👍".into(), true),
                Action::React(id, "👍".into(), false)
            ]
        );
        call.react(99, "👍");
        assert_eq!(call.outbox.len(), 2, "no such message");
    }

    #[test]
    fn others_count_once_per_emoji() {
        let now = Instant::now();
        let (mut call, id) = live_with_message(now);
        call.reacted(PRIYA, "Priya".into(), id, "🎉", true);
        call.reacted(PRIYA, "Priya".into(), id, "🎉", true);
        call.reacted(SAM, "Sam".into(), id, "🎉", true);
        call.react(id, "🎉");
        let reaction = &call.chat[0].reactions[0];
        assert_eq!(reaction.by.len(), 3);
        call.reacted(PRIYA, "Priya".into(), id, "🎉", false);
        call.reacted(SAM, "Sam".into(), id, "😂", false);
        assert_eq!(call.chat[0].reactions[0].by.len(), 2);
        call.reacted(SAM, "Sam".into(), 12345, "🎉", true);
        assert_eq!(call.chat[0].reactions.len(), 1, "unknown message");
    }

    #[test]
    fn a_message_holds_a_bounded_number_of_emoji() {
        let now = Instant::now();
        let (mut call, id) = live_with_message(now);
        for emoji in PICKER {
            call.reacted(PRIYA, "Priya".into(), id, emoji, true);
        }
        assert_eq!(call.chat[0].reactions.len(), MAX_EMOJI_PER_MESSAGE);
    }

    #[test]
    fn old_messages_take_no_reactions() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", false, now);
        call.receive("Old".into(), None, "hi".into(), now);
        call.reacted(PRIYA, "Priya".into(), 0, "👍", true);
        assert!(call.chat[0].reactions.is_empty());
    }

    #[test]
    fn offered_emoji_are_valid_reactions() {
        for emoji in PICKER.iter().chain(QUICK_REACTIONS.iter()) {
            assert!(veil_call::AppMessage::react(1, emoji, true).is_ok(), "{emoji}");
            assert_eq!(emoji.chars().count(), 1, "{emoji} is one character");
        }
    }
}
