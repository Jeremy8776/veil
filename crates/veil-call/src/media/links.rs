//! Setting up one media link: dialling a member, and accepting the links members dial.
//!
//! Invariants (see the parent module): an accepted link is used only once the routes expect
//! its peer; an early one waits up to `EARLY_WAIT`, and at most `MAX_EARLY` wait at once.

use std::net::SocketAddr;

use tokio::sync::mpsc;
use tokio::time::Instant;
use veil_net::Node;
use veil_net::control::Control;
use veil_security::PublicId;

use super::{Carrier, Ctx, EARLY_WAIT, Extra, MAX_EARLY, State};
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
                    if state.early.len() < MAX_EARLY {
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
