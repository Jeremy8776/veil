//! Links again with a peer whose media link dropped mid-call, for example after a Wi-Fi blip.
//!
//! Purpose: without this, links are only dialled when routes change, so a member whose link
//! dropped stayed "Connecting" until someone joined or left. [`super::MediaSession::redial`]
//! runs on the app's once-a-second tick and dials every wanted peer with no link, as the
//! same side that dialled it first.
//!
//! Invariants:
//! - Only the side that dials a pair (`crate::routes::dials`) redials it; the other side keeps
//!   expecting the peer and accepts its new link as usual. A forwarder link is joined again
//!   with the newest ticket this member was given.
//! - At most one redial per peer at a time. After a failure the next try waits [`FIRST_GAP`],
//!   doubling up to [`MAX_GAP`], so a peer that is gone for good costs little.
//! - A redial is adopted only if the peer is still wanted and has no link by then.
//! - [`LinkEvent`]s say when a link dropped, when it came back, and the first failed redial of
//!   each outage, so the app can log them without logging every retry.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use veil_forward::ForwarderClient;
use veil_net::{Node, NodeAddr};
use veil_security::PublicId;

use super::links::open_media;
use super::{Carrier, Ctx, Extra, State};
use crate::CallError;
use crate::node::Path;
use crate::presence::Presence;
use crate::routes::Peer;

/// Wait after the first failed redial.
pub const FIRST_GAP: Duration = Duration::from_secs(1);
/// The longest wait between redials of one peer.
pub const MAX_GAP: Duration = Duration::from_secs(16);

/// A change to one media link worth telling the user about.
#[derive(Debug)]
pub enum LinkEvent {
    /// The link to this peer closed while the routes still wanted it.
    Dropped(PublicId),
    /// A dropped link is up again after this long.
    Back { peer: PublicId, after: Duration },
    /// The first redial after a drop failed. Later failures of the same outage are not told.
    StillDown(PublicId, CallError),
}

/// How to reach the peers this member dials, from the newest routes.
#[derive(Default)]
pub(super) enum Plan {
    #[default]
    None,
    /// The members this side dials, by key.
    Mesh(HashMap<PublicId, Peer>),
    Forwarder { addr: NodeAddr, ticket: Vec<u8> },
}

/// One peer's redial state.
pub(super) struct Retry {
    busy: bool,
    next: Instant,
    gap: Duration,
}

/// What a redial needs to run outside the state lock.
pub(super) struct Dialer {
    pub node: Arc<Node>,
    pub presence: Option<Presence>,
    pub relay_only: bool,
    pub room_id: [u8; 16],
}

enum Target {
    Member(Peer),
    Forwarder { addr: NodeAddr, ticket: Vec<u8> },
}

/// Starts a redial for each wanted peer that has no link and is due.
pub(super) fn start(ctx: &Ctx, dialer: &Dialer) {
    let now = Instant::now();
    let due: Vec<(PublicId, Target)> = {
        let Some(mut state) = ctx.lock() else { return };
        let State { wanted, links, plan, retry, .. } = &mut *state;
        let mut due = Vec::new();
        for peer in wanted.iter().filter(|p| !links.contains_key(*p)) {
            let target = match plan {
                Plan::Mesh(peers) => peers.get(peer).cloned().map(Target::Member),
                Plan::Forwarder { addr, ticket } if addr.key == *peer => {
                    Some(Target::Forwarder { addr: *addr, ticket: ticket.clone() })
                }
                _ => None,
            };
            let Some(target) = target else { continue };
            let entry = retry.entry(*peer).or_insert(Retry { busy: false, next: now, gap: FIRST_GAP });
            if entry.busy || entry.next > now {
                continue;
            }
            entry.busy = true;
            due.push((*peer, target));
        }
        due
    };
    for (peer, target) in due {
        let ctx = ctx.clone();
        let (node, presence) = (dialer.node.clone(), dialer.presence.clone());
        let (relay_only, room_id) = (dialer.relay_only, dialer.room_id);
        tokio::spawn(async move {
            let result = match target {
                Target::Member(p) => open_media(&node, presence.as_ref(), &p, relay_only, room_id).await,
                Target::Forwarder { addr, ticket } => {
                    ForwarderClient::join(&node, &[addr.addr], addr.key, &ticket)
                        .await
                        .map(|client| {
                            let extra = Extra { input: None, via: None };
                            (Carrier::Forwarder(client), Path::Forwarder, extra)
                        })
                        .map_err(CallError::Forward)
                }
            };
            finish(&ctx, peer, result);
        });
    }
}

/// Adopts a redial's link, or schedules the next try.
fn finish(ctx: &Ctx, peer: PublicId, result: Result<(Carrier, Path, Extra), CallError>) {
    let Some(mut state) = ctx.lock() else { return };
    match result {
        Ok((carrier, path, extra)) => {
            state.retry.remove(&peer);
            // A dropped carrier closes its link: unwanted now, or linked another way meanwhile.
            if state.wanted.contains(&peer) && !state.links.contains_key(&peer) {
                ctx.adopt(&mut state, peer, carrier, path, extra);
            }
        }
        Err(e) => {
            let Some(retry) = state.retry.get_mut(&peer) else { return };
            let first = retry.gap == FIRST_GAP;
            retry.busy = false;
            retry.next = Instant::now() + retry.gap;
            retry.gap = next_gap(retry.gap);
            if first {
                state.events.push(LinkEvent::StillDown(peer, e));
            }
        }
    }
    drop(state);
    ctx.changed();
}

/// The wait after the next failure, given the wait after this one.
fn next_gap(gap: Duration) -> Duration {
    (gap * 2).min(MAX_GAP)
}

/// Whether a redial of `peer` is under way, so a new plan does not dial it twice.
pub(super) fn busy(state: &State, peer: &PublicId) -> bool {
    state.retry.get(peer).is_some_and(|r| r.busy)
}

/// Records a wanted link that closed, so its return can be timed.
pub(super) fn dropped(state: &mut State, peer: PublicId) {
    state.lost.insert(peer, Instant::now());
    state.events.push(LinkEvent::Dropped(peer));
}

/// Records a link to `peer` coming up; tells it only if the peer had dropped.
pub(super) fn linked(state: &mut State, peer: PublicId) {
    if let Some(at) = state.lost.remove(&peer) {
        state.events.push(LinkEvent::Back { peer, after: at.elapsed() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gap_doubles_up_to_the_cap() {
        let mut gap = FIRST_GAP;
        let mut seen = Vec::new();
        for _ in 0..7 {
            seen.push(gap.as_secs());
            gap = next_gap(gap);
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 16, 16]);
    }

    #[test]
    fn only_a_dropped_link_reports_coming_back() {
        let peer = veil_security::Identity::generate().public();
        let mut state = State::default();
        linked(&mut state, peer);
        assert!(state.events.is_empty(), "a first link is not news");
        dropped(&mut state, peer);
        linked(&mut state, peer);
        assert!(matches!(state.events[..], [LinkEvent::Dropped(_), LinkEvent::Back { .. }]));
        assert!(state.lost.is_empty());
    }
}
