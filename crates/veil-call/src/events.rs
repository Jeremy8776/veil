//! Ordered application events with a reserved terminal slot. Never wait on the consumer:
//! the consumer may itself be waiting for a command response from the session actor.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

/// Maximum pending nonterminal events per host/member. One additional slot is reserved
/// for the terminal overload event. Overflow ends the session rather than losing keys.
pub const EVENT_BACKLOG: usize = 64;

pub(crate) struct EventSender<T> {
    sender: mpsc::Sender<T>,
    stopped: AtomicBool,
}

impl<T> EventSender<T> {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<T>) {
        let (sender, receiver) = mpsc::channel(EVENT_BACKLOG + 1);
        (
            Self {
                sender,
                stopped: AtomicBool::new(false),
            },
            receiver,
        )
    }

    /// Single producer only; no sender clones or externally reserved permits.
    pub(crate) fn emit(&self, event: T) -> bool {
        if self.is_stopped() {
            return false;
        }
        if self.sender.capacity() <= 1 || self.sender.try_send(event).is_err() {
            self.stopped.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    pub(crate) async fn closed(&self) {
        self.sender.closed().await;
    }

    /// Called once at teardown, including when the normal event budget was exhausted.
    pub(crate) fn finish(&self, event: T) {
        let _ = self.sender.try_send(event);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn overload_keeps_order_and_a_terminal_slot_without_waiting() {
        let (events, mut receiver) = EventSender::channel();
        for event in 0..EVENT_BACKLOG {
            assert!(events.emit(event));
        }
        for _ in 0..10_000 {
            assert!(!events.emit(999));
        }
        assert_eq!(receiver.len(), EVENT_BACKLOG);
        assert_eq!(receiver.recv().await, Some(0));
        assert!(
            !events.emit(999),
            "draining cannot revive an overloaded session"
        );
        events.finish(EVENT_BACKLOG);
        drop(events);
        for event in 1..=EVENT_BACKLOG {
            assert_eq!(receiver.recv().await, Some(event));
        }
        assert_eq!(receiver.recv().await, None);
    }
}
