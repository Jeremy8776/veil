//! Reachability check: can someone outside dial this node's public address?
//!
//! Purpose: a host that opened a port, on its router or by hand, asks a rendezvous node to dial
//! it back ([`ClientMsg::CheckReach`](crate::protocol::ClientMsg::CheckReach)). The node dials
//! from a fresh socket with a throwaway key, so the answer is not let through only because the
//! requester's own link to the node already opened its NAT for that one address and port.
//!
//! Invariants:
//! - The node dials only the requester's own reflected IP, never an address it names, so a
//!   check cannot be aimed at anyone else. The requester chooses the port only.
//! - The dial pins the requester's key, so another machine at that address does not count.
//! - One check at a time per link, at most [`MAX_PER_LINK`] per link, and [`MAX_RUNNING`] for
//!   the whole server. Each dial is bounded by [`DIAL_LIMIT`].
//!
//! shortcut: the dial comes from the node's own IP. A NAT that filters by IP only, or not at
//! all, lets it in without an open port, so "open" can be a false positive there. A second
//! node on another IP would rule that out; add one alongside the optional default node.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use veil_net::{Link, Node};
use veil_security::{Identity, PublicId};

use crate::protocol::{Refusal, ServerMsg};

/// Checks running at once on one server.
pub const MAX_RUNNING: usize = 32;
/// Checks one link may ask for in its life.
pub const MAX_PER_LINK: u32 = 8;
/// How long the node waits for the requester's handshake.
pub const DIAL_LIMIT: Duration = Duration::from_secs(5);

/// One link's checks: whether one is running, and how many it has asked for.
#[derive(Default)]
pub(crate) struct LinkChecks {
    busy: Arc<AtomicBool>,
    asked: u32,
}

impl LinkChecks {
    /// Starts a check of `link`'s reflected IP at `port`. The answer, or a refusal, goes to
    /// `outbox`.
    pub(crate) fn start(
        &mut self,
        running: &Arc<Semaphore>,
        link: &Link,
        port: u16,
        outbox: &mpsc::Sender<ServerMsg>,
    ) {
        let refuse = |r| {
            let _ = outbox.try_send(ServerMsg::Refused(r));
        };
        if port == 0 {
            return refuse(Refusal::BadPort);
        }
        if self.asked >= MAX_PER_LINK || self.busy.load(Ordering::Acquire) {
            return refuse(Refusal::Full);
        }
        let Ok(permit) = running.clone().try_acquire_owned() else {
            return refuse(Refusal::Full);
        };
        self.asked += 1;
        self.busy.store(true, Ordering::Release);
        let busy = self.busy.clone();
        let target = target(link.remote_addr(), port);
        let (key, outbox) = (link.peer(), outbox.clone());
        tokio::spawn(async move {
            let open = dial(target, key).await;
            drop(permit);
            busy.store(false, Ordering::Release);
            let _ = outbox.try_send(ServerMsg::Reach { addr: target, open });
        });
    }
}

/// The address to dial: the reflected IP, as plain IPv4 when it is IPv4-mapped, at `port`.
fn target(reflected: SocketAddr, port: u16) -> SocketAddr {
    SocketAddr::new(reflected.ip().to_canonical(), port)
}

/// Whether a handshake with `key` at `target` completes, dialled from a fresh socket.
async fn dial(target: SocketAddr, key: PublicId) -> bool {
    let any = if target.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let Ok(node) = Node::bind(&Identity::generate(), any) else {
        return false;
    };
    let open = matches!(
        tokio::time::timeout(DIAL_LIMIT, node.connect(&[target], key)).await,
        Ok(Ok(_))
    );
    node.close();
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dials_the_reflected_ip_only_at_the_asked_port() {
        let reflected = SocketAddr::from(([203, 0, 113, 9], 50_123));
        assert_eq!(
            target(reflected, 7710),
            SocketAddr::from(([203, 0, 113, 9], 7710))
        );
        let mapped: SocketAddr = "[::ffff:203.0.113.9]:50123".parse().unwrap_or(reflected);
        assert_eq!(
            target(mapped, 7710),
            SocketAddr::from(([203, 0, 113, 9], 7710))
        );
    }
}
