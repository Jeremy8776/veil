//! Finding a path to a peer: direct, punched and relayed, started in stages.
//!
//! Purpose: a person on mobile data is usually behind carrier-grade NAT, where direct dials
//! fail only after their whole timeout. Trying one path after another could take about half a
//! minute, so the paths overlap. The direct dial starts at once, the punched dial after
//! [`PUNCH_AFTER`], and the relay after [`RELAY_AFTER`]. The first link up is used.
//!
//! Invariants:
//! - A path that has failed starts the next one at once.
//! - A call that connects directly within [`PUNCH_AFTER`] asks the rendezvous for nothing, and
//!   one that connects within [`RELAY_AFTER`] opens no relay session, so the relay does not
//!   learn about pairs that never need it.
//! - Links from losing paths never open a control stream, and dropping them closes them, so
//!   the peer routes only the winner.
//! - A relay-only node uses only the relay, so the peer never learns its address.
//! - A punched dial goes only to a peer this node chose (the invite's host, or a member the
//!   host's signed plan names), at the helper's addresses for it, filtered by the presence
//!   task (`dial_back::punch_targets`).
//!   Requiring an endpoint already seen on this node made a first punch impossible, so two
//!   people behind routers could not reach each other without the relay.
//!
//! shortcut: a link stays on the path it started on, so a slow direct path that would have
//! worked after [`RELAY_AFTER`] loses to the relay for that link. Move media to a better path
//! during the call if relayed links prove common.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;
use veil_net::{Link, NetError, Node};
use veil_rendezvous::punch;
use veil_rendezvous::socket::RELAY_PEER;
use veil_security::PublicId;

use crate::CallError;
use crate::node::Path;
use crate::presence::Presence;

/// The direct dial's head start over the punched dial.
const PUNCH_AFTER: Duration = Duration::from_secs(1);
/// How long the other paths have before the relay is tried too.
const RELAY_AFTER: Duration = Duration::from_secs(5);

/// A dialled link and what keeps it running.
pub(crate) struct Dialled {
    pub link: Link,
    /// The virtual node of a relayed link. Must live as long as the link.
    pub via: Option<Arc<Node>>,
    pub path: Path,
}

/// Dials `peer` over the paths this node may use and returns the first link up.
pub(crate) async fn dial(
    node: &Node,
    presence: Option<&Presence>,
    peer: PublicId,
    addrs: &[SocketAddr],
    relay_only: bool,
) -> Result<Dialled, CallError> {
    let direct = async {
        let link = node.connect(addrs, peer).await?;
        Ok(Dialled { link, via: None, path: Path::Direct })
    };
    let Some(presence) = presence else {
        return match relay_only {
            true => Err(NetError::NoAddresses.into()),
            false => direct.await,
        };
    };
    let relayed = async {
        let inner = presence.relay(peer).await?;
        let link = inner.connect(&[RELAY_PEER], peer).await?;
        Ok(Dialled { link, via: Some(inner), path: Path::Relayed })
    };
    if relay_only {
        return relayed.await;
    }
    let punched = async {
        let plan = presence.introduce(peer).await?;
        if plan.addrs.is_empty() { return Err(NetError::NoAddresses.into()); }
        let link = punch(node, &plan).await?;
        Ok(Dialled { link, via: None, path: Path::Punched })
    };
    first_path(direct, punched, relayed).await
}

/// The first success of the three paths, each started as the module describes, or the last
/// error once all have failed. A path's future does nothing until it is started.
async fn first_path<T, E>(
    direct: impl Future<Output = Result<T, E>>,
    punched: impl Future<Output = Result<T, E>>,
    relayed: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    tokio::pin!(direct, punched, relayed);
    let punch_at = sleep(PUNCH_AFTER);
    let relay_at = sleep(RELAY_AFTER);
    tokio::pin!(punch_at, relay_at);
    let (mut direct_live, mut punch_live, mut relay_live) = (true, true, true);
    let (mut punch_on, mut relay_on) = (false, false);
    loop {
        punch_on |= !direct_live;
        relay_on |= !direct_live && !punch_live;
        let result = tokio::select! {
            result = &mut direct, if direct_live => {
                direct_live = false;
                result
            }
            result = &mut punched, if punch_live && punch_on => {
                punch_live = false;
                result
            }
            result = &mut relayed, if relay_live && relay_on => {
                relay_live = false;
                result
            }
            () = &mut punch_at, if !punch_on => {
                punch_on = true;
                continue;
            }
            () = &mut relay_at, if !relay_on => {
                relay_on = true;
                continue;
            }
        };
        match result {
            Ok(value) => return Ok(value),
            Err(e) if !direct_live && !punch_live && !relay_live => return Err(e),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::time::Instant;

    use super::*;

    type Outcome = Result<&'static str, &'static str>;

    /// A path that notes when it starts, takes `after` (forever if `None`), then gives `outcome`.
    async fn path(started: &AtomicBool, after: Option<u64>, outcome: Outcome) -> Outcome {
        started.store(true, Ordering::Relaxed);
        match after {
            Some(ms) => sleep(Duration::from_millis(ms)).await,
            None => std::future::pending().await,
        }
        outcome
    }

    #[derive(Default)]
    struct Started {
        direct: AtomicBool,
        punched: AtomicBool,
        relayed: AtomicBool,
    }

    #[tokio::test(start_paused = true)]
    async fn a_quick_direct_link_starts_nothing_else() {
        let s = Started::default();
        let won = first_path(
            path(&s.direct, Some(100), Ok("direct")),
            path(&s.punched, None, Ok("punched")),
            path(&s.relayed, None, Ok("relayed")),
        )
        .await;
        assert_eq!(won, Ok("direct"));
        assert!(!s.punched.load(Ordering::Relaxed));
        assert!(!s.relayed.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn a_punch_can_win_while_direct_still_waits() {
        let s = Started::default();
        let start = Instant::now();
        let won = first_path(
            path(&s.direct, None, Ok("direct")),
            path(&s.punched, Some(500), Ok("punched")),
            path(&s.relayed, None, Ok("relayed")),
        )
        .await;
        assert_eq!(won, Ok("punched"));
        assert_eq!(start.elapsed(), PUNCH_AFTER + Duration::from_millis(500));
        assert!(!s.relayed.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn the_relay_joins_when_the_other_paths_hang() {
        let s = Started::default();
        let start = Instant::now();
        let won = first_path(
            path(&s.direct, None, Ok("direct")),
            path(&s.punched, None, Ok("punched")),
            path(&s.relayed, Some(200), Ok("relayed")),
        )
        .await;
        assert_eq!(won, Ok("relayed"));
        assert_eq!(start.elapsed(), RELAY_AFTER + Duration::from_millis(200));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_path_starts_the_next_at_once() {
        let s = Started::default();
        let start = Instant::now();
        let won = first_path(
            path(&s.direct, Some(10), Err("no direct")),
            path(&s.punched, Some(10), Err("no punch")),
            path(&s.relayed, Some(10), Ok("relayed")),
        )
        .await;
        assert_eq!(won, Ok("relayed"));
        assert_eq!(start.elapsed(), Duration::from_millis(30));
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_error_is_returned_when_all_fail() {
        let s = Started::default();
        let lost = first_path(
            path(&s.direct, Some(1), Err("no direct")),
            path(&s.punched, Some(1), Err("no punch")),
            path(&s.relayed, Some(1), Err("no relay")),
        )
        .await;
        assert_eq!(lost, Err("no relay"));
    }
}
