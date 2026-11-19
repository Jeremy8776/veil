//! Byte budgets for queued video: per receiver, for the whole node and per remote address
//! block. See the parent module's header for how a frame's [`Reservation`] uses them.
//!
//! Invariant: a reservation holds its bytes against all three budgets at once, and gives them
//! back exactly once, when it is dropped.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::Limits;

/// Queued video bytes that are counted across receivers.
#[derive(Default)]
pub(super) struct Budgets {
    node: AtomicUsize,
    /// Per remote address block. An entry is removed when it drops to zero.
    sources: Mutex<HashMap<IpAddr, usize>>,
}

impl Budgets {
    fn reserve_source(&self, source: IpAddr, len: usize, budget: usize) -> bool {
        let Ok(mut sources) = self.sources.lock() else {
            return false;
        };
        let used = sources.get(&source).copied().unwrap_or(0);
        match used.checked_add(len).filter(|total| *total <= budget) {
            Some(total) => {
                if total > 0 {
                    sources.insert(source, total);
                }
                true
            }
            None => false,
        }
    }

    fn release_source(&self, source: IpAddr, len: usize) {
        let mut sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(used) = sources.get_mut(&source) {
            *used = used.saturating_sub(len);
            if *used == 0 {
                sources.remove(&source);
            }
        }
    }
}

/// Reads a running forwarder's queued video total. For monitoring and tests.
#[derive(Clone)]
pub struct QueueMonitor(pub(super) Arc<Budgets>);

impl QueueMonitor {
    /// Video bytes now queued for all receivers.
    pub fn queued_bytes(&self) -> usize {
        self.0.node.load(Ordering::Acquire)
    }
}

/// `len` bytes held against a receiver's, the node's and a source block's budgets. Dropping it
/// gives all three back.
pub(super) struct Reservation {
    len: usize,
    budgets: Arc<Budgets>,
    queued: Arc<AtomicUsize>,
    source: IpAddr,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        release(&self.queued, self.len);
        release(&self.budgets.node, self.len);
        self.budgets.release_source(self.source, self.len);
    }
}

/// Reserves `len` bytes from the receiver's, the node's and the source block's budgets, all or
/// nothing.
pub(super) fn reserve_all(
    budgets: &Arc<Budgets>,
    queued: &Arc<AtomicUsize>,
    source: IpAddr,
    len: usize,
    limits: &Limits,
) -> Option<Reservation> {
    if !reserve(queued, len, limits.queue_bytes) {
        return None;
    }
    if !reserve(&budgets.node, len, limits.node_queue_bytes) {
        release(queued, len);
        return None;
    }
    if !budgets.reserve_source(source, len, limits.source_queue_bytes) {
        release(&budgets.node, len);
        release(queued, len);
        return None;
    }
    Some(Reservation { len, budgets: budgets.clone(), queued: queued.clone(), source })
}

/// Reserves `len` bytes of one atomic budget. Atomic, so concurrent senders in one room
/// cannot overshoot the budget together.
fn reserve(queued: &AtomicUsize, len: usize, budget: usize) -> bool {
    queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |now| {
            now.checked_add(len).filter(|total| *total <= budget)
        })
        .is_ok()
}

fn release(queued: &AtomicUsize, len: usize) {
    let _ = queued.fetch_update(Ordering::AcqRel, Ordering::Acquire, |now| Some(now.saturating_sub(len)));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn byte_budget_refuses_frames_past_the_limit_and_frees_on_send() {
        let queued = AtomicUsize::new(0);
        assert!(reserve(&queued, 40, 100));
        assert!(reserve(&queued, 60, 100));
        assert!(!reserve(&queued, 1, 100), "budget is full");
        assert_eq!(queued.load(Ordering::Acquire), 100);
        release(&queued, 60);
        assert!(reserve(&queued, 50, 100));
        assert!(!reserve(&queued, usize::MAX, 100), "no overflow");
        assert_eq!(queued.load(Ordering::Acquire), 90);
    }

    #[test]
    fn reserve_all_is_all_or_nothing_and_releases_on_drop() {
        let budgets = Arc::new(Budgets::default());
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let ip: IpAddr = [10, 0, 0, 1].into();
        let other: IpAddr = [10, 0, 0, 2].into();
        let limits = Limits { queue_bytes: 100, node_queue_bytes: 150, source_queue_bytes: 120, ..Limits::default() };

        let r1 = reserve_all(&budgets, &a, ip, 100, &limits).unwrap();
        // The node budget (150) refuses; the receiver and source counts roll back.
        assert!(reserve_all(&budgets, &b, other, 60, &limits).is_none());
        assert_eq!(b.load(Ordering::Acquire), 0);
        assert_eq!(budgets.node.load(Ordering::Acquire), 100);
        assert!(!budgets.sources.lock().unwrap().contains_key(&other));
        // The source budget (120) refuses for the same block.
        assert!(reserve_all(&budgets, &b, ip, 30, &limits).is_none());
        assert_eq!(b.load(Ordering::Acquire), 0);
        assert_eq!(budgets.node.load(Ordering::Acquire), 100);
        drop(r1);
        assert_eq!(a.load(Ordering::Acquire), 0);
        assert_eq!(budgets.node.load(Ordering::Acquire), 0);
        assert!(budgets.sources.lock().unwrap().is_empty(), "entries vanish at zero");
    }
}
