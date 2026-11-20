//! The call's media links, set up from the topology plan.
//!
//! Purpose: [`MediaSession::apply`] takes this participant's [`Routes`] and makes the links
//! match: a link to every other member in a mesh, or one link to the home forwarder. Sealed
//! frames then go out on every link with [`MediaSession::send_frame`] and
//! [`MediaSession::send_audio`], and everything that arrives comes out of
//! [`MediaSession::recv`]. Frames stay sealed here; open them with a [`crate::KeyRing`].
//! [`MediaSession::request_keyframe`] asks one sender for a keyframe: on the media link's
//! control stream in a mesh, through the forwarder otherwise. [`MediaSession::send_app`]
//! sends a sealed app message (names, chat; see [`crate::app_msg`]) the same way to everyone.
//!
//! Invariants:
//! - One link per pair of members: [`crate::routes::dials`] picks which one dials (the one
//!   without a known address dials the one with; otherwise the lower key), the other accepts.
//!   Both still dial for a punch, but the accepting side's dial is only for its NAT.
//! - An accepted media link is used only if its pinned key is a member the current routes
//!   expect. One that arrives early (before this member's epoch change) waits up to
//!   [`EARLY_WAIT`]; past that it is closed. Places to wait are kept for the epoch's members
//!   ([`MediaSession::members`]); other keys share [`MAX_EARLY_UNKNOWN`] (`media/links.rs`).
//! - Frames are never opened or changed: the media pipeline seals and opens them.
//! - A wanted link that drops mid-call is dialled again by [`MediaSession::redial`], by the
//!   side that dialled it first, with a growing wait between failures (`media/redial.rs`).
//! - No send waits on one peer without a deadline, and video does not wait at all: each link
//!   has its own writer task (`media/write.rs`, GHSA-37f5-22p2-q6hr).
//!
//! shortcut: a forwarder link is direct only. Forwarders run on reachable nodes; upgrade with
//! relay fallback if forwarders behind NAT become common.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use veil_forward::{ForwarderClient, Wanted};
use veil_net::control::{Control, MAX_APP_LEN};
use veil_net::{FramedRecv, FramedSend, Link, Node, NodeAddr};
use veil_security::PublicId;

use crate::inbound::Inbound;
use crate::node::Path;
use crate::presence::Presence;
use crate::routes::{Peer, Routes, dials};
use crate::CallError;

mod links;
mod read;
mod redial;
mod stats;
mod write;

use links::{adopt_early, open_media, run_inbox};
pub use redial::LinkEvent;
pub use stats::LinkStats;
pub use write::SEND_DEADLINE;

/// How long an early media link may wait for this member's routes to expect it.
pub const EARLY_WAIT: Duration = Duration::from_secs(10);
const MAX_EARLY: usize = 16;
/// Early links from keys not in the newest known roster, all together. A member that is one
/// epoch behind its peer needs these; anyone with an old invite can fill them.
const MAX_EARLY_UNKNOWN: usize = 4;
const INCOMING: usize = 1_024;

/// A sealed packet as it arrived, or a request about this member's own video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Video { frame_id: u32, sealed: Vec<u8> },
    Audio(Bytes),
    /// Someone cannot decode this member's video until its next keyframe. Unauthenticated:
    /// a forwarder or member can send it at will, so the sender limits how often it obeys
    /// (`veil_media::Schedule::request_keyframe`).
    KeyframeRequest,
    /// A sealed app message (a name or chat). Open it with the `KeyRing`, then
    /// [`crate::AppMessage::decode`].
    App(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The link's peer: the sending member in a mesh, the forwarder otherwise. The true
    /// sender is the frame's authenticated sender index.
    pub via: PublicId,
    pub media: Media,
}

enum Carrier {
    /// A link to another member, with its media control stream.
    Member { link: Link, control: tokio::sync::Mutex<FramedSend> },
    Forwarder(ForwarderClient),
}

impl Carrier {
    fn member(link: Link, control: FramedSend) -> Self {
        Self::Member {
            link,
            control: tokio::sync::Mutex::new(control),
        }
    }

    fn link(&self) -> &Link {
        match self {
            Self::Member { link, .. } => link,
            Self::Forwarder(client) => client.link(),
        }
    }
}

struct Entry {
    id: u64,
    carrier: Arc<Carrier>,
    path: Path,
    _via: Option<Arc<Node>>,
    /// The link's video queue, drained by its writer task.
    frames: mpsc::Sender<write::Frame>,
    /// Readers and the writer.
    readers: [JoinHandle<()>; 4],
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
    /// The newest epoch's members, from [`MediaSession::members`].
    members: HashSet<PublicId>,
    next_id: u64,
    /// How to reach the peers this side dials, kept for redials (`media/redial.rs`).
    plan: redial::Plan,
    retry: HashMap<PublicId, redial::Retry>,
    /// Wanted peers whose link dropped, and when.
    lost: HashMap<PublicId, Instant>,
    /// Untold link changes, taken by [`MediaSession::take_events`].
    events: Vec<LinkEvent>,
}

/// What reader tasks and the inbox task share.
#[derive(Clone)]
struct Ctx {
    state: Arc<Mutex<State>>,
    incoming: mpsc::Sender<Incoming>,
    closed: mpsc::Sender<(PublicId, u64)>,
    changes: Arc<watch::Sender<u64>>,
    /// This member's device key.
    me: PublicId,
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
        let control = match rest.input {
            Some(input) => tokio::spawn(read::member_control(input, peer, self.incoming.clone())),
            None => tokio::spawn(read::forwarder_control(carrier.clone(), peer, self.incoming.clone())),
        };
        let (frames, writer) = write::spawn_writer(carrier.clone());
        let readers = [
            tokio::spawn(read::video(carrier.clone(), peer, id, self.clone())),
            tokio::spawn(read::audio(carrier.clone(), peer, self.incoming.clone())),
            control,
            writer,
        ];
        let entry = Entry {
            id,
            carrier,
            path,
            _via: rest.via,
            frames,
            readers,
        };
        state.links.insert(peer, entry);
        redial::linked(state, peer);
    }
}

struct Extra {
    /// A member link's control stream, read for keyframe requests. `None` for a forwarder.
    input: Option<FramedRecv>,
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
            // An in-flight redial holds a Ctx too. It must not adopt a link after this
            // session has gone away, even if it connects successfully a moment later.
            state.wanted.clear();
            state.expected.clear();
            state.plan = redial::Plan::None;
            state.retry.clear();
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
            me,
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

    /// The members of the newest epoch (`EpochKeys::roster`). Call it on every new epoch,
    /// before applying that epoch's routes: their early links are kept ahead of other keys'.
    pub fn members(&self, devices: impl IntoIterator<Item = PublicId>) {
        if let Some(mut state) = self.ctx.lock() {
            state.members = devices.into_iter().collect();
        }
    }

    /// Makes the links match `routes`. Links no longer wanted are closed. Returns the peers
    /// this member had to dial but could not reach, with why.
    pub async fn apply(&mut self, routes: Routes) -> Vec<(PublicId, CallError)> {
        match routes {
            Routes::Mesh { peers, reachable } => self.apply_mesh(peers, reachable).await,
            Routes::Forwarder { addr, ticket } => self.apply_forwarder(addr, &ticket).await,
        }
    }

    async fn apply_mesh(
        &mut self,
        peers: Vec<Peer>,
        reachable: bool,
    ) -> Vec<(PublicId, CallError)> {
        // Only the members these routes link to may be introduced or relayed to this one.
        if let Some(presence) = &self.presence {
            presence.expect(peers.iter().map(|p| p.device));
        }
        let mut to_dial = Vec::new();
        if let Some(mut state) = self.ctx.lock() {
            state.wanted = peers.iter().map(|p| p.device).collect();
            state.expected = peers
                .iter()
                .filter(|p| !dials(&self.me, reachable, p))
                .map(|p| p.device)
                .collect();
            let mine = peers.iter().filter(|p| dials(&self.me, reachable, p));
            state.plan = redial::Plan::Mesh(mine.map(|p| (p.device, p.clone())).collect());
            let State { links, wanted, retry, lost, .. } = &mut *state;
            links.retain(|peer, _| wanted.contains(peer));
            retry.retain(|peer, _| wanted.contains(peer));
            lost.retain(|peer, _| wanted.contains(peer));
            adopt_early(&self.ctx, &mut state);
            to_dial = peers
                .into_iter()
                .filter(|p| {
                    dials(&self.me, reachable, p)
                        && !state.links.contains_key(&p.device)
                        && !redial::busy(&state, &p.device)
                })
                .collect();
        }
        self.ctx.changed();

        let mut dialling = JoinSet::new();
        for peer in to_dial {
            let (node, presence) = (self.node.clone(), self.presence.clone());
            let (relay_only, room_id) = (self.relay_only, self.room_id);
            dialling.spawn(async move {
                let result = open_media(&node, presence.as_ref(), &peer, relay_only, room_id).await;
                (peer.device, result)
            });
        }
        let mut failed = Vec::new();
        while let Some(done) = dialling.join_next().await {
            let Ok((device, result)) = done else { continue };
            match result {
                Ok((carrier, path, extra)) => {
                    if let Some(mut state) = self.ctx.lock()
                        && state.wanted.contains(&device)
                    {
                        self.ctx.adopt(&mut state, device, carrier, path, extra);
                    }
                    self.ctx.changed();
                }
                Err(e) => failed.push((device, e)),
            }
        }
        failed
    }

    async fn apply_forwarder(
        &mut self,
        addr: NodeAddr,
        ticket: &[u8],
    ) -> Vec<(PublicId, CallError)> {
        // Through a forwarder no member dials this one.
        if let Some(presence) = &self.presence {
            presence.expect([]);
        }
        let have = self.ctx.lock().and_then(|mut state| {
            state.wanted = HashSet::from([addr.key]);
            state.expected.clear();
            state.early.clear();
            state.links.retain(|peer, _| *peer == addr.key);
            state.retry.retain(|peer, _| *peer == addr.key);
            state.lost.retain(|peer, _| *peer == addr.key);
            state.plan = redial::Plan::Forwarder { addr, ticket: ticket.to_vec() };
            state.links.get(&addr.key).map(|e| e.carrier.clone())
        });
        self.ctx.changed();
        if let Some(carrier) = have {
            // Already linked: hand on this epoch's ticket so the forwarder drops removed members.
            let Carrier::Forwarder(client) = &*carrier else {
                return Vec::new();
            };
            return match client.update_ticket(ticket).await {
                Ok(()) => Vec::new(),
                Err(e) => vec![(addr.key, CallError::Forward(e))],
            };
        }
        if self.ctx.lock().is_some_and(|s| redial::busy(&s, &addr.key)) {
            return Vec::new(); // A redial is joining it already, with this ticket.
        }
        let joined = ForwarderClient::join(&self.node, &[addr.addr], addr.key, ticket).await;
        match joined {
            Ok(client) => {
                if let Some(mut state) = self.ctx.lock() {
                    let extra = Extra { input: None, via: None };
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

    /// Dials again every wanted peer whose link dropped and is due a try (`media/redial.rs`).
    /// Returns at once; a redial that works is adopted like any other link. Call it on the
    /// app's once-a-second tick.
    pub fn redial(&self) {
        let dialer = redial::Dialer {
            node: self.node.clone(),
            presence: self.presence.clone(),
            relay_only: self.relay_only,
            room_id: self.room_id,
        };
        redial::start(&self.ctx, &dialer);
    }

    /// Closes every link as if the network had dropped. For tests of [`Self::redial`].
    #[doc(hidden)]
    pub fn cut_links_for_test(&self) {
        for carrier in self.carriers() {
            carrier.link().close("cut for a test");
        }
    }

    /// Link drops, returns and failed redials since the last call, oldest first.
    pub fn take_events(&self) -> Vec<LinkEvent> {
        self.ctx.lock().map(|mut s| std::mem::take(&mut s.events)).unwrap_or_default()
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

    /// Queues one sealed video frame for every link. Never waits on a peer: a link whose queue
    /// is full skips this frame. Returns how many links queued it.
    pub async fn send_frame(&self, frame_id: u32, sealed: &[u8]) -> usize {
        let sealed: Arc<[u8]> = Arc::from(sealed);
        let queues: Vec<_> = self.ctx.lock().map_or_else(Vec::new, |s| {
            s.links.values().map(|e| e.frames.clone()).collect()
        });
        queues
            .iter()
            .filter(|q| q.try_send((frame_id, sealed.clone())).is_ok())
            .count()
    }

    /// Sends one sealed audio packet on every link. Returns how many links took it.
    pub fn send_audio(&self, sealed: &[u8]) -> usize {
        self.carriers()
            .iter()
            .filter(|c| c.link().send_datagram(sealed).is_ok())
            .count()
    }

    /// Sends one sealed app message to every member: on each member link's control stream in
    /// a mesh, through the forwarder otherwise. Returns how many links took it. A message
    /// over `MAX_APP_LEN` is not sent. Links are sent to side by side, each within
    /// [`SEND_DEADLINE`], so this returns within the deadline whatever any peer does.
    pub async fn send_app(&self, sealed: &[u8]) -> usize {
        if sealed.len() > MAX_APP_LEN {
            return 0;
        }
        let sealed: Arc<[u8]> = Arc::from(sealed);
        let mut sends = JoinSet::new();
        for carrier in self.carriers() {
            let sealed = sealed.clone();
            sends.spawn(async move {
                match &*carrier {
                    Carrier::Member { .. } => {
                        write::control(&carrier, &Control::App(sealed.to_vec())).await.is_ok()
                    }
                    Carrier::Forwarder(client) => write::forwarder(client.send_app(&sealed)).await.is_ok(),
                }
            });
        }
        let mut sent = 0;
        while let Some(done) = sends.join_next().await {
            if matches!(done, Ok(true)) {
                sent += 1;
            }
        }
        sent
    }

    /// Asks the forwarder for at most these layers from each named member, for example
    /// [`Wanted::THUMB`] for a small tile. Does nothing without a forwarder link.
    ///
    /// shortcut: in a mesh every layer still arrives. Add a member-to-member request if mesh
    /// calls send more than one layer. A new forwarder link starts with every layer again, so
    /// the app subscribes again after routes change.
    pub async fn subscribe(&self, request: &[(PublicId, Wanted)]) -> Result<(), CallError> {
        for carrier in self.carriers() {
            if let Carrier::Forwarder(client) = &*carrier {
                client.subscribe(request).await.map_err(CallError::Forward)?;
            }
        }
        Ok(())
    }

    /// Asks `sender` for a keyframe: on its own link in a mesh, through the forwarder
    /// otherwise. Returns `false` if no link could carry the request.
    pub async fn request_keyframe(&self, sender: PublicId) -> Result<bool, CallError> {
        let carrier = self.ctx.lock().and_then(|s| {
            s.links.get(&sender).map(|e| e.carrier.clone()).or_else(|| {
                s.links
                    .values()
                    .find(|e| matches!(*e.carrier, Carrier::Forwarder(_)))
                    .map(|e| e.carrier.clone())
            })
        });
        match carrier.as_deref() {
            None => Ok(false),
            Some(member @ Carrier::Member { .. }) => {
                write::control(member, &Control::KeyframeRequest).await?;
                Ok(true)
            }
            Some(Carrier::Forwarder(client)) => {
                write::forwarder(client.request_keyframe(sender)).await?;
                Ok(true)
            }
        }
    }

    /// The next packet from any link. `None` once the session has stopped.
    pub async fn recv(&mut self) -> Option<Incoming> {
        self.incoming.recv().await
    }
}
