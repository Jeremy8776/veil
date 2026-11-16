//! Setting up one media link: dialling a member, and accepting the links members dial.
//!
//! Invariants (see the parent module): an accepted link is used only once the routes expect
//! its peer; an early one waits up to `EARLY_WAIT`. Only a lower key may wait (a higher key's
//! link is never used: this member dials it). Members of the newest epoch this session was
//! told about ([`super::MediaSession::members`]) share `MAX_EARLY` places; any other key gets
//! one of `MAX_EARLY_UNKNOWN`, so someone holding an old invite cannot crowd members out.

use std::collections::HashSet;
use std::net::SocketAddr;

use tokio::sync::mpsc;
use tokio::time::Instant;
use veil_net::Node;
use veil_net::control::Control;
use veil_security::PublicId;

use super::{Carrier, Ctx, EARLY_WAIT, Extra, MAX_EARLY, MAX_EARLY_UNKNOWN, State};
use crate::CallError;
use crate::inbound::Inbound;
use crate::node::{Path, dial};
use crate::presence::Presence;
use crate::routes::Peer;

/// Dials a member and opens the media control stream.
pub(super) async fn open_media(
    node: &Node,
    presence: Option<&Presence>,
    peer: &Peer,
    relay_only: bool,
    room_id: [u8; 16],
) -> Result<(Carrier, Path, Extra), CallError> {
    let addrs: &[SocketAddr] = &peer.addrs;
    let dialled = dial(node, presence, peer.device, addrs, relay_only).await?;
    let (mut out, input) = dialled.link.open_control().await?.split();
    out.send(&Control::Media { room_id }).await?;
    let extra = Extra {
        input: Some(input),
        via: dialled.via,
    };
    Ok((Carrier::member(dialled.link, out), dialled.path, extra))
}

/// Moves early links whose peer is now expected into the link set; drops stale ones.
pub(super) fn adopt_early(ctx: &Ctx, state: &mut State) {
    let now = Instant::now();
    state.early.retain(|_, (_, at)| now.duration_since(*at) < EARLY_WAIT);
    let ready: Vec<PublicId> = state
        .early
        .keys()
        .filter(|p| state.expected.contains(p))
        .copied()
        .collect();
    for peer in ready {
        if let Some((inbound, _)) = state.early.remove(&peer) {
            accept(ctx, state, inbound);
        }
    }
}

fn accept(ctx: &Ctx, state: &mut State, inbound: Inbound) {
    let peer = inbound.link.peer();
    let path = if inbound.via.is_some() { Path::Relayed } else { Path::Direct };
    let extra = Extra {
        input: Some(inbound.input),
        via: inbound.via,
    };
    ctx.adopt(state, peer, Carrier::member(inbound.link, inbound.out), path, extra);
}

pub(super) async fn run_inbox(
    ctx: Ctx,
    mut inbox: mpsc::Receiver<Inbound>,
    mut closed: mpsc::Receiver<(PublicId, u64)>,
) {
    loop {
        tokio::select! {
            inbound = inbox.recv() => {
                let Some(inbound) = inbound else { break };
                let Some(mut state) = ctx.lock() else { break };
                let peer = inbound.link.peer();
                if state.expected.contains(&peer) {
                    accept(&ctx, &mut state, inbound);
                } else {
                    adopt_early(&ctx, &mut state);
                    if may_wait(ctx.me, &state.members, state.early.keys(), peer) {
                        state.early.insert(peer, (inbound, Instant::now()));
                    }
                }
            }
            gone = closed.recv() => {
                let Some((peer, id)) = gone else { break };
                let Some(mut state) = ctx.lock() else { break };
                if state.links.get(&peer).is_some_and(|e| e.id == id) {
                    state.links.remove(&peer);
                }
            }
        }
        ctx.changed();
    }
}

/// Whether an early link from `peer` may wait, given the keys already waiting.
fn may_wait<'a>(
    me: PublicId,
    members: &HashSet<PublicId>,
    waiting: impl Iterator<Item = &'a PublicId>,
    peer: PublicId,
) -> bool {
    if peer.0 >= me.0 {
        return false;
    }
    let (mut known, mut unknown) = (0, 0);
    for key in waiting.filter(|k| **k != peer) {
        if members.contains(key) {
            known += 1;
        } else {
            unknown += 1;
        }
    }
    if members.contains(&peer) {
        known < MAX_EARLY
    } else {
        unknown < MAX_EARLY_UNKNOWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicId {
        PublicId([byte; 32])
    }

    #[test]
    fn strangers_cannot_crowd_out_members() {
        let me = key(200);
        let members: HashSet<PublicId> = [key(1), key(2)].into();
        let strangers: Vec<PublicId> = (10..10 + MAX_EARLY_UNKNOWN as u8).map(key).collect();
        assert!(!may_wait(me, &members, strangers.iter(), key(99)), "stranger pool full");
        assert!(may_wait(me, &members, strangers.iter(), key(1)), "a member still waits");
        assert!(may_wait(me, &members, strangers[1..].iter(), key(99)));
    }

    #[test]
    fn a_higher_key_never_waits() {
        let members: HashSet<PublicId> = [key(9)].into();
        assert!(!may_wait(key(5), &members, std::iter::empty(), key(9)));
        assert!(!may_wait(key(5), &members, std::iter::empty(), key(5)));
    }

    #[test]
    fn members_share_the_large_pool() {
        let me = key(250);
        let members: HashSet<PublicId> = (0..=MAX_EARLY as u8).map(key).collect();
        let waiting: Vec<PublicId> = (0..MAX_EARLY as u8).map(key).collect();
        assert!(!may_wait(me, &members, waiting.iter(), key(MAX_EARLY as u8)));
        assert!(may_wait(me, &members, waiting.iter(), key(0)), "a key already waiting is replaced");
    }
}
