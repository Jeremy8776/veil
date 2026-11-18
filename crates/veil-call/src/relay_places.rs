//! The relay sessions other members opened to this one, while each waits for its link.
//!
//! A relay session carries one link. Its task waits for that link and then ends, so a place is
//! held only for that short wait, whether or not the session is ever used.
//!
//! Invariants:
//! - At most [`MAX_WAITING`] sessions wait at once, and [`MAX_PER_PEER`] from one peer.
//! - A new session always gets a place. It replaces that peer's oldest one if the peer is at
//!   its limit, otherwise the oldest of all if the set is full. So sessions nobody uses cannot
//!   shut out the next member, and a peer that asks repeatedly replaces only its own.
//! - The tasks end with this set.

use std::collections::VecDeque;
use std::future::Future;

use tokio::task::{AbortHandle, JoinSet};
use veil_security::PublicId;

/// Sessions waiting for a link. Sixteen is the call's size limit, and a member may be setting
/// up two links at once.
const MAX_WAITING: usize = 32;
/// One peer's waiting sessions. It needs two at once (its control and media links); the rest
/// leave room for retries.
const MAX_PER_PEER: usize = 4;

#[derive(Default)]
pub(crate) struct RelayPlaces {
    tasks: JoinSet<()>,
    /// Oldest first.
    waiting: VecDeque<(PublicId, AbortHandle)>,
}

impl RelayPlaces {
    /// Runs `task`, the wait for `peer`'s link, in a place of its own.
    pub fn start(&mut self, peer: PublicId, task: impl Future<Output = ()> + Send + 'static) {
        while self.tasks.try_join_next().is_some() {}
        self.waiting.retain(|(_, handle)| !handle.is_finished());
        let theirs = self.waiting.iter().filter(|(p, _)| *p == peer).count();
        let replaced = if theirs >= MAX_PER_PEER {
            self.waiting.iter().position(|(p, _)| *p == peer)
        } else if self.waiting.len() >= MAX_WAITING {
            Some(0)
        } else {
            None
        };
        if let Some((_, handle)) = replaced.and_then(|i| self.waiting.remove(i)) {
            handle.abort();
        }
        let handle = self.tasks.spawn(task);
        self.waiting.push_back((peer, handle));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    fn key(n: u8) -> PublicId {
        PublicId([n; 32])
    }

    struct Report(usize, mpsc::UnboundedSender<usize>);

    impl Drop for Report {
        fn drop(&mut self) {
            let _ = self.1.send(self.0);
        }
    }

    /// A task that waits for ever and reports `id` when it is stopped.
    async fn waiter(id: usize, ended: mpsc::UnboundedSender<usize>) {
        let _report = Report(id, ended);
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn one_peer_replaces_only_its_own_oldest_session() {
        let (tx, mut ended) = mpsc::unbounded_channel();
        let mut places = RelayPlaces::default();
        places.start(key(9), waiter(100, tx.clone()));
        for id in 0..MAX_PER_PEER {
            places.start(key(1), waiter(id, tx.clone()));
        }
        tokio::task::yield_now().await;
        assert!(ended.try_recv().is_err(), "everyone is within the limits");

        places.start(key(1), waiter(50, tx.clone()));
        assert_eq!(ended.recv().await, Some(0));
        assert_eq!(places.waiting.len(), MAX_PER_PEER + 1, "the other peer keeps its place");
    }

    #[tokio::test]
    async fn a_full_set_gives_the_oldest_place_to_a_new_peer() {
        let (tx, mut ended) = mpsc::unbounded_channel();
        let mut places = RelayPlaces::default();
        for id in 0..MAX_WAITING {
            places.start(key(u8::try_from(id).unwrap()), waiter(id, tx.clone()));
        }
        tokio::task::yield_now().await;
        places.start(key(200), waiter(99, tx.clone()));
        assert_eq!(ended.recv().await, Some(0));
        assert_eq!(places.waiting.len(), MAX_WAITING);
    }

    #[tokio::test]
    async fn a_finished_wait_gives_its_place_back() {
        let (tx, mut ended) = mpsc::unbounded_channel();
        let mut places = RelayPlaces::default();
        for _ in 0..MAX_PER_PEER {
            places.start(key(1), async {});
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        places.start(key(2), waiter(1, tx.clone()));
        assert_eq!(places.waiting.len(), 1);
        assert!(ended.try_recv().is_err());
    }

    #[tokio::test]
    async fn dropping_the_set_stops_every_wait() {
        let (tx, mut ended) = mpsc::unbounded_channel();
        let mut places = RelayPlaces::default();
        places.start(key(1), waiter(7, tx.clone()));
        tokio::task::yield_now().await;
        drop(places);
        assert_eq!(ended.recv().await, Some(7));
    }
}
