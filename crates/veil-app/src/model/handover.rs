//! A persistent call's handover as the interface sees it (ADR 0010).
//!
//! Purpose: what the leave dialog, the Available to host switch and the in-call notices show.
//! The engine fills [`Handover`] (`engine/host/hosting.rs`, `engine/guest.rs`); these methods
//! only queue the person's choices for it, or in a demo do nothing.
//!
//! Invariants:
//! - The list shows names and a Good/OK/Poor bar only, never figures or addresses.
//! - Available to host stays off while Hide my address is on.

use super::{Action, Call};

/// The bar beside each person who can take over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Good,
    Ok,
    Poor,
}

impl Bar {
    pub fn label(self) -> &'static str {
        match self {
            Self::Good => "Good",
            Self::Ok => "OK",
            Self::Poor => "Poor",
        }
    }
}

/// One person the leaving host may pick, best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Successor {
    pub id: u32,
    pub name: String,
    pub bar: Bar,
}

/// Where a host's handover stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Idle,
    /// Waiting for the named person to be ready, or for the old host's removal.
    Handing(String),
    /// Nobody could take over. The host is still host.
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handover {
    /// The call outlives its host.
    pub persistent: bool,
    /// Host: who can take over, best first.
    pub choices: Vec<Successor>,
    pub stage: Stage,
    /// Member: the Available to host switch.
    pub available: bool,
    /// Member: Hide my address is on, so the switch is disabled.
    pub hide_address: bool,
    /// Relay-only member: the new host's name, while Veil asks before changing relay.
    pub relay_ask: Option<String>,
    /// One line to show once, such as "Ana is now the host."
    pub notice: Option<String>,
    /// Host: people in the lobby who were removed or declined earlier in this call. Admitting
    /// them needs a second confirm.
    pub removed_earlier: Vec<u32>,
}

impl Call {
    /// Host of a persistent call: hand over to `pick`, or with `None` to the best connection.
    pub fn hand_over(&mut self, pick: Option<u32>) {
        if !self.demo {
            self.outbox.push(Action::HandOver(pick));
        }
    }

    /// Host: end the call for everyone, also in a persistent call.
    pub fn end_for_everyone(&mut self) {
        if !self.demo {
            self.outbox.push(Action::EndCall);
        }
    }

    /// Member of a persistent call: turn Available to host on or off.
    pub fn set_available(&mut self, on: bool) {
        let on = on && !self.handover.hide_address;
        self.handover.available = on;
        if !self.demo {
            self.outbox.push(Action::AvailableToHost(on));
        }
    }

    /// Relay-only member: stay (`true`) with the new host's relay, or leave.
    pub fn answer_relay(&mut self, stay: bool) {
        self.handover.relay_ask = None;
        if !self.demo {
            self.outbox.push(Action::RelayChange(stay));
        }
    }

    /// The leave dialog is needed: a real host of a persistent call.
    pub fn asks_before_leaving(&self) -> bool {
        self.is_host && self.handover.persistent && !self.demo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn hide_my_address_keeps_available_off() {
        let mut call = Call::live("t".into(), "Ana", false, Instant::now());
        call.handover.hide_address = true;
        call.set_available(true);
        assert!(!call.handover.available);
        assert_eq!(call.outbox, vec![Action::AvailableToHost(false)]);
    }

    #[test]
    fn only_a_persistent_host_is_asked_before_leaving() {
        let mut call = Call::live("t".into(), "Ana", true, Instant::now());
        assert!(!call.asks_before_leaving());
        call.handover.persistent = true;
        assert!(call.asks_before_leaving());
        call.is_host = false;
        assert!(!call.asks_before_leaving());
    }

    #[test]
    fn choices_queue_for_the_engine() {
        let mut call = Call::live("t".into(), "Ana", true, Instant::now());
        call.hand_over(Some(3));
        call.hand_over(None);
        call.end_for_everyone();
        assert_eq!(
            call.outbox,
            vec![Action::HandOver(Some(3)), Action::HandOver(None), Action::EndCall]
        );
    }
}
