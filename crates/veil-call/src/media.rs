//! The call's media links, set up from the topology plan.
//!
//! Purpose: [`MediaSession::apply`] takes this participant's [`Routes`] and makes the links
//! match: a link to every other member in a mesh, or one link to the home forwarder. Sealed
//! frames then go out on every link with [`MediaSession::send_frame`] and
//! [`MediaSession::send_audio`], and everything that arrives comes out of
//! [`MediaSession::recv`]. Frames stay sealed here; open them with a [`crate::KeyRing`].
//!
//! Invariants:
//! - One link per pair of members: the member with the lower device key dials, the other
//!   accepts. Both still dial for a punch, but the higher key's dial is only for its NAT.
//! - An accepted media link is used only if its pinned key is a member the current routes
//!   expect. One that arrives early (before this member's epoch change) waits up to
//!   [`EARLY_WAIT`]; past that, or past [`MAX_EARLY`] links, it is closed.
//! - Frames are never opened or changed: the media pipeline seals and opens them.
//!
//! shortcut: a forwarder link is direct only. Forwarders run on reachable nodes; upgrade with
//! relay fallback if forwarders behind NAT become common.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use veil_forward::ForwarderClient;
use veil_net::control::Control;
use veil_net::{FramedRecv, FramedSend, Link, Node, NodeAddr};
use veil_rendezvous::room_tag;
use veil_security::PublicId;

use crate::inbound::Inbound;
use crate::node::{Path, dial};
use crate::presence::Presence;
use crate::routes::{Peer, Routes};
use crate::CallError;

/// How long an early media link may wait for this member's routes to expect it.
pub const EARLY_WAIT: Duration = Duration::from_secs(10);
const MAX_EARLY: usize = 16;
const INCOMING: usize = 1_024;

/// A sealed packet as it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Video { frame_id: u32, sealed: Vec<u8> },
    Audio(Bytes),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The link's peer: the sending member in a mesh, the forwarder otherwise. The true
    /// sender is the frame's authenticated sender index.
    pub via: PublicId,
    pub media: Media,
}

enum Carrier {
    Link(Link),
    Forwarder(ForwarderClient),
}

impl Carrier {
    fn link(&self) -> &Link {
        match self {
            Self::Link(link) => link,
            Self::Forwarder(client) => client.link(),
        }
    }
}

struct Entry {
    id: u64,
    carrier: Arc<Carrier>,
    path: Path,
    /// A member link's control stream, kept open for the link's life.
    _control: Option<(FramedSend, FramedRecv)>,
    _via: Option<Arc<Node>>,
    readers: [JoinHandle<()>; 2],
}

impl Drop for Entry {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        self.carrier.link().close("media link closed");
    }
}

#[derive(Default)]
struct State {
    links: HashMap<PublicId, Entry>,
    /// Peers the current routes want a link with.
    wanted: HashSet<PublicId>,
    /// Members that dial this one, so their links are accepted.
    expected: HashSet<PublicId>,
    early: HashMap<PublicId, (Inbound, Instant)>,
    next_id: u64,
}

/// What reader tasks and the inbox task share.
#[derive(Clone)]
struct Ctx {
    state: Arc<Mutex<State>>,
    incoming: mpsc::Sender<Incoming>,
    closed: mpsc::Sender<(PublicId, u64)>,
    changes: Arc<watch::Sender<u64>>,
}

impl Ctx {
    fn lock(&self) -> Option<MutexGuard<'_, State>> {
        // Poisoned means a task panicked holding it; stop using the state.
        self.state.lock().ok()
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Starts readers for a link and stores it, replacing any older link to the same peer.
    fn adopt(&self, state: &mut State, peer: PublicId, carrier: Carrier, path: Path, rest: Extra) {
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        let carrier = Arc::new(carrier);
        let readers = [
            tokio::spawn(read_video(carrier.clone(), peer, id, self.clone())),
            tokio::spawn(read_audio(carrier.clone(), peer, self.incoming.clone())),
        ];
        let entry = Entry {
            id,
            carrier,
            path,
            _control: rest.control,
            _via: rest.via,
            readers,
        };
        state.links.insert(peer, entry);
    }
}

struct Extra {
    control: Option<(FramedSend, FramedRecv)>,
    via: Option<Arc<Node>>,
}

pub struct MediaSession {
    node: Arc<Node>,
    me: PublicId,
    room_id: [u8; 16],
    presence: Option<Presence>,
    relay_only: bool,
    ctx: Ctx,
    incoming: mpsc::Receiver<Incoming>,
    inbox_task: JoinHandle<()>,
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        self.inbox_task.abort();
        if let Some(mut state) = self.ctx.lock() {
            state.links.clear();
            state.early.clear();
        }
    }
}

impl MediaSession {
    pub(crate) fn new(
        node: Arc<Node>,
        me: PublicId,
        room_id: [u8; 16],
        presence: Option<Presence>,
        relay_only: bool,
        inbox: mpsc::Receiver<Inbound>,
    ) -> Self {
        let (incoming_tx, incoming) = mpsc::channel(INCOMING);
        let (closed_tx, closed_rx) = mpsc::channel(INCOMING);
        let ctx = Ctx {
            state: Arc::default(),
            incoming: incoming_tx,
            closed: closed_tx,
            changes: Arc::new(watch::channel(0).0),
        };
        let inbox_task = tokio::spawn(run_inbox(ctx.clone(), inbox, closed_rx));
        Self {
            node,
            me,
            room_id,
            presence,
            relay_only,
            ctx,
            incoming,
            inbox_task,
        }
    }

    /// Makes the links match `routes`. Links no longer wanted are closed. Returns the peers
    /// this member had to dial but could not reach, with why.
    pub async fn apply(&mut self, routes: Routes) -> Vec<(PublicId, CallError)> {
        match routes {
            Routes::Mesh(peers) => self.apply_mesh(peers).await,
            Routes::Forwarder(addr) => self.apply_forwarder(addr).await,
        }
    }

    async fn apply_mesh(&mut self, peers: Vec<Peer>) -> Vec<(PublicId, CallError)> {
        let mut to_dial = Vec::new();
        if let Some(mut state) = self.ctx.lock() {
            state.wanted = peers.iter().map(|p| p.device).collect();
            state.expected = peers
                .iter()
                .map(|p| p.device)
                .filter(|d| d.0 < self.me.0)
                .collect();
            let State { links, wanted, .. } = &mut *state;
            links.retain(|peer, _| wanted.contains(peer));
            adopt_early(&self.ctx, &mut state);
            to_dial = peers
                .into_iter()
                .filter(|p| self.me.0 < p.device.0 && !state.links.contains_key(&p.device))
                .collect();
        }
        self.ctx.changed();

        let mut dials = JoinSet::new();
        for peer in to_dial {
            let (node, presence) = (self.node.clone(), self.presence.clone());
            let (relay_only, room_id) = (self.relay_only, self.room_id);
            dials.spawn(async move {
                let result = open_media(&node, presence.as_ref(), &peer, relay_only, room_id).await;
                (peer.device, result)
            });
        }
        let mut failed = Vec::new();
        while let Some(done) = dials.join_next().await {
            let Ok((device, result)) = done else { continue };
            match result {
                Ok((link, path, extra)) => {
                    if let Some(mut state) = self.ctx.lock()
                        && state.wanted.contains(&device)
                    {
                        self.ctx.adopt(&mut state, device, Carrier::Link(link), path, extra);
                    }
                    self.ctx.changed();
                }
                Err(e) => failed.push((device, e)),
            }
        }
        failed
    }

    async fn apply_forwarder(&mut self, addr: NodeAddr) -> Vec<(PublicId, CallError)> {
        let have = self.ctx.lock().is_some_and(|mut state| {
            state.wanted = HashSet::from([addr.key]);
            state.expected.clear();
            state.early.clear();
            state.links.retain(|peer, _| *peer == addr.key);
            state.links.contains_key(&addr.key)
        });
        self.ctx.changed();
        if have {
            return Vec::new();
        }
        let joined =
            ForwarderClient::join(&self.node, &[addr.addr], addr.key, room_tag(&self.room_id)).await;
        match joined {
            Ok(client) => {
                if let Some(mut state) = self.ctx.lock() {
                    let extra = Extra { control: None, via: None };
                    let carrier = Carrier::Forwarder(client);
                    self.ctx.adopt(&mut state, addr.key, carrier, Path::Forwarder, extra);
                }
                self.ctx.changed();
                Vec::new()
            }
            Err(e) => vec![(addr.key, CallError::Forward(e))],
        }
    }

    /// Waits until every peer the routes want has a link, or `wait` passes.
    pub async fn ready(&self, wait: Duration) -> Result<(), CallError> {
        let deadline = Instant::now() + wait;
        let mut changes = self.ctx.changes.subscribe();
        loop {
            let complete = self
                .ctx
                .lock()
                .is_some_and(|s| s.wanted.iter().all(|p| s.links.contains_key(p)));
            if complete {
                return Ok(());
            }
            tokio::time::timeout_at(deadline, changes.changed())
                .await
                .map_err(|_| CallError::Timeout)?
                .map_err(|_| CallError::Closed)?;
        }
    }

    /// Each linked peer and the path its link took.
    pub fn paths(&self) -> Vec<(PublicId, Path)> {
        self.ctx.lock().map_or_else(Vec::new, |s| {
            s.links.iter().map(|(peer, e)| (*peer, e.path)).collect()
        })
    }

    fn carriers(&self) -> Vec<Arc<Carrier>> {
        self.ctx.lock().map_or_else(Vec::new, |s| {
            s.links.values().map(|e| e.carrier.clone()).collect()
        })
    }

    /// Sends one sealed video frame on every link at once. Returns how many links took it.
    pub async fn send_frame(&self, frame_id: u32, sealed: &[u8]) -> usize {
        let sealed: Arc<[u8]> = Arc::from(sealed);
        let mut sends = JoinSet::new();
        for carrier in self.carriers() {
            let sealed = sealed.clone();
            sends.spawn(async move { carrier.link().send_frame(frame_id, &sealed).await.is_ok() });
        }
        let mut sent = 0;
        while let Some(done) = sends.join_next().await {
            if matches!(done, Ok(true)) {
                sent += 1;
            }
        }
        sent
    }

    /// Sends one sealed audio packet on every link. Returns how many links took it.
    pub fn send_audio(&self, sealed: &[u8]) -> usize {
        self.carriers()
            .iter()
            .filter(|c| c.link().send_datagram(sealed).is_ok())
            .count()
    }

    /// The next packet from any link. `None` once the session has stopped.
    pub async fn recv(&mut self) -> Option<Incoming> {
        self.incoming.recv().await
    }
}

/// Dials a member and opens the media control stream.
async fn open_media(
    node: &Node,
    presence: Option<&Presence>,
    peer: &Peer,
    relay_only: bool,
    room_id: [u8; 16],
) -> Result<(Link, Path, Extra), CallError> {
    let addrs: &[SocketAddr] = &peer.addrs;
    let dialled = dial(node, presence, peer.device, addrs, relay_only).await?;
    let (mut out, input) = dialled.link.open_control().await?.split();
    out.send(&Control::Media { room_id }).await?;
    let extra = Extra {
        control: Some((out, input)),
        via: dialled.via,
    };
    Ok((dialled.link, dialled.path, extra))
}

/// Moves early links whose peer is now expected into the link set; drops stale ones.
fn adopt_early(ctx: &Ctx, state: &mut State) {
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
        control: Some((inbound.out, inbound.input)),
        via: inbound.via,
    };
    ctx.adopt(state, peer, Carrier::Link(inbound.link), path, extra);
}

async fn run_inbox(
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

async fn read_video(carrier: Arc<Carrier>, via: PublicId, id: u64, ctx: Ctx) {
    while let Ok((frame_id, sealed)) = carrier.link().recv_frame().await {
        let media = Media::Video { frame_id, sealed };
        if ctx.incoming.send(Incoming { via, media }).await.is_err() {
            return;
        }
    }
    let _ = ctx.closed.send((via, id)).await;
}

async fn read_audio(carrier: Arc<Carrier>, via: PublicId, incoming: mpsc::Sender<Incoming>) {
    while let Ok(packet) = carrier.link().recv_datagram().await {
        // Audio is late or lost, never queued: a full channel drops the packet.
        let _ = incoming.try_send(Incoming {
            via,
            media: Media::Audio(packet),
        });
    }
}
