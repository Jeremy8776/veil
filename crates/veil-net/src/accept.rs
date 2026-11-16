//! Accepting links without letting one peer stall or exhaust the node.
//!
//! Purpose: a task per [`crate::Node`] takes every incoming QUIC connection, runs its
//! handshake on a task of its own with a deadline, and hands finished [`Link`]s to
//! `Node::accept` through a bounded channel. Before this, `accept` waited for each handshake
//! in turn, so one peer that sent a first packet and went quiet (from a spoofed address, for
//! free) stopped every other link for the idle timeout.
//!
//! Invariants:
//! - At most [`MAX_HANDSHAKES`] handshakes run at once; past that a new connection is refused.
//! - Each handshake gets [`HANDSHAKE_TIMEOUT`], then it is dropped.
//! - When more than half the handshake slots are busy, a connection whose address is not
//!   yet proven gets a QUIC Retry first, so spoofed addresses cannot hold slots.
//! - At most [`MAX_LINKS_PER_IP`] links (handshaking or live) per remote address block and
//!   [`MAX_LINKS`] in all. A slot is freed when its connection closes. A block is one IPv4
//!   address, or one IPv6 /64: a single home or host is usually given a whole /64, so
//!   counting each IPv6 address apart would let one machine take every slot.
//! - The task ends when the endpoint closes; `Node` aborts it when dropped.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};

use crate::endpoint::Link;

pub const MAX_HANDSHAKES: usize = 256;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
/// Several people behind one home or office router share an IP; 64 leaves room for them.
pub const MAX_LINKS_PER_IP: usize = 64;
pub const MAX_LINKS: usize = 8_192;
/// Finished links waiting for `Node::accept`.
const READY: usize = 64;

/// The address block a remote IP is counted under: the IPv4 address itself (an IPv4-mapped
/// IPv6 address counts as its IPv4 address), or the /64 an IPv6 address belongs to.
pub(crate) fn block(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mut octets = v6.octets();
                octets[8..].fill(0);
                IpAddr::V6(octets.into())
            }
        },
    }
}

/// Live link counts, per remote address block (see [`block`]) and in all.
#[derive(Default)]
pub(crate) struct Counts {
    per_ip: HashMap<IpAddr, usize>,
    total: usize,
}

/// One counted link. Dropping it gives the slot back.
pub(crate) struct Slot {
    counts: Arc<Mutex<Counts>>,
    ip: IpAddr,
}

impl Slot {
    /// Takes a slot for `ip`, or `None` if its address block or the node is full.
    pub(crate) fn take(counts: &Arc<Mutex<Counts>>, ip: IpAddr) -> Option<Self> {
        let ip = block(ip);
        let mut c = counts.lock().ok()?;
        let mine = c.per_ip.get(&ip).copied().unwrap_or(0);
        if mine >= MAX_LINKS_PER_IP || c.total >= MAX_LINKS {
            return None;
        }
        c.per_ip.insert(ip, mine + 1);
        c.total += 1;
        Some(Self {
            counts: counts.clone(),
            ip,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let Ok(mut c) = self.counts.lock() else {
            return;
        };
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_ip.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per_ip.remove(&self.ip);
            }
        }
    }
}

/// Starts the accept task for `endpoint`. Finished links arrive on the receiver.
pub(crate) fn start(
    endpoint: quinn::Endpoint,
) -> (mpsc::Receiver<Link>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(READY);
    (rx, tokio::spawn(run(endpoint, tx)))
}

async fn run(endpoint: quinn::Endpoint, ready: mpsc::Sender<Link>) {
    let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    let counts = Arc::new(Mutex::new(Counts::default()));
    while let Some(incoming) = endpoint.accept().await {
        let busy = MAX_HANDSHAKES - handshakes.available_permits();
        if busy > MAX_HANDSHAKES / 2 && !incoming.remote_address_validated() {
            // The peer must echo the Retry token from its real address before it costs more.
            let _ = incoming.retry();
            continue;
        }
        let Some(slot) = Slot::take(&counts, incoming.remote_address().ip()) else {
            incoming.refuse();
            continue;
        };
        let Ok(permit) = handshakes.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let ready = ready.clone();
        tokio::spawn(async move {
            let Ok(connecting) = incoming.accept() else {
                return;
            };
            let conn = tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting).await;
            drop(permit);
            let Ok(Ok(conn)) = conn else {
                return;
            };
            let Ok(link) = Link::new(conn.clone()) else {
                return;
            };
            // The slot lives exactly as long as the connection.
            tokio::spawn(async move {
                conn.closed().await;
                drop(slot);
            });
            // Nobody accepting any more: the link is dropped and closes.
            let _ = ready.send(link).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_capped_per_ip_and_given_back() {
        let counts = Arc::new(Mutex::new(Counts::default()));
        let (a, b) = (IpAddr::from([192, 0, 2, 1]), IpAddr::from([192, 0, 2, 2]));
        let held: Vec<Slot> = (0..MAX_LINKS_PER_IP)
            .filter_map(|_| Slot::take(&counts, a))
            .collect();
        assert_eq!(held.len(), MAX_LINKS_PER_IP);
        assert!(Slot::take(&counts, a).is_none(), "one IP is capped");
        assert!(Slot::take(&counts, b).is_some(), "another IP is not");
        drop(held);
        assert!(Slot::take(&counts, a).is_some(), "slots come back");
        let c = counts
            .lock()
            .map(|c| (c.total, c.per_ip.len()))
            .unwrap_or_default();
        assert_eq!(c, (0, 0), "every dropped slot is counted out");
    }

    #[test]
    fn one_ipv6_slash_64_shares_one_cap() {
        let counts = Arc::new(Mutex::new(Counts::default()));
        let held: Vec<Slot> = (0..MAX_LINKS_PER_IP)
            .filter_map(|i| {
                let host = u64::try_from(i).unwrap_or(0) + 1;
                let ip = std::net::Ipv6Addr::from((0x2001_0db8_0000_0001_u128 << 64) | u128::from(host));
                Slot::take(&counts, IpAddr::V6(ip))
            })
            .collect();
        assert_eq!(held.len(), MAX_LINKS_PER_IP);
        let same_64: IpAddr = "2001:db8:0:1:ffff::9".parse().unwrap_or(IpAddr::from([0; 4]));
        let next_64: IpAddr = "2001:db8:0:2::1".parse().unwrap_or(IpAddr::from([0; 4]));
        assert!(Slot::take(&counts, same_64).is_none(), "the /64 is capped");
        assert!(Slot::take(&counts, next_64).is_some(), "another /64 is not");
    }

    #[test]
    fn mapped_ipv4_counts_as_ipv4() {
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap_or(IpAddr::from([0; 4]));
        assert_eq!(block(mapped), IpAddr::from([192, 0, 2, 7]));
    }
}
