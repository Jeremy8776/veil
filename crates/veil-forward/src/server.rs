//! The forwarding service.
//!
//! State lives behind one mutex that is never held across an `.await`. Each member link has a
//! control reader (which also detects the link closing), a video reader, an audio reader and a
//! video writer. Video for a member waits in a queue bounded in bytes; when the queue is full,
//! new frames for that member are dropped.
//!
//! Queued video is also bounded for the whole node ([`Limits::node_queue_bytes`]) and per
//! remote address block of the receiving links ([`Limits::source_queue_bytes`], blocks as in
//! `veil_net::accept::block`), so many rooms cannot add up to more than the node can hold. A
//! frame holds a reservation against all three budgets from the moment it is queued until it
//! is sent, dropped or its queue is dropped; the reservation gives the bytes back when it is
//! dropped, so they are released exactly once on every path. A frame that waits longer than
//! [`Limits::send_deadline`], or whose send takes longer than that, is dropped: a receiver
//! that never reads cannot pin memory.
//!
//! Layers: each receiver's [`Subscriptions`] decide which SVC layers of each sender it gets,
//! read from the frame's plaintext layer marker. A frame whose marker does not parse is dropped.
//!
//! Keyframe requests: a member's `KeyframeRequest(sender)` goes on to that sender, without
//! the asker's identity. A receiver whose subscription raise waits for a keyframe makes the
//! forwarder ask the sender itself. Each member's control queue is bounded; when it is full,
//! requests are dropped (the sender coalesces them anyway).
//!
//! App messages: a member's sealed `App` (names, chat) goes unchanged to every other member's
//! control queue. The forwarder holds no keys, so it cannot open them; it sees only their size.
//!
//! Admission: `Join` carries a host-signed [`RosterTicket`]. Rooms are keyed by (room tag,
//! host key); only listed devices join, epochs only move forward, and a newer ticket (on a
//! join or a later `Ticket` message from any member) drops members it no longer lists.
//!
//! Serving one host: a participant's own forwarder runs for its own call only
//! ([`ForwarderServer::only_host`]). It refuses tickets signed by any other key, so strangers
//! who find its port cannot use the participant's upload for their own rooms.
//!
//! shortcut: no forwarder-to-forwarder cascade yet; one forwarder serves a whole room.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;
use veil_net::accept::block;
use veil_net::control::MAX_APP_LEN;
use veil_net::{FramedSend, Link, NetError, Node};
use veil_security::PublicId;
use veil_security::frame::frame_layer;

use crate::protocol::{ClientMsg, Refusal, ServerMsg};
use crate::subscribe::{Subscriptions, Wanted};
use crate::ticket::RosterTicket;

/// Frames waiting for one receiver, counted in frames. The byte budget usually binds first.
const VIDEO_QUEUE: usize = 256;
const REFUSAL_LINGER: std::time::Duration = std::time::Duration::from_secs(2);
/// Control messages waiting for one member.
const CONTROL_QUEUE: usize = 16;
/// A link must send its `Join` within this time.
const JOIN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_rooms: usize,
    pub max_members_per_room: usize,
    /// Video bytes allowed to wait for one receiver before frames are dropped.
    pub queue_bytes: usize,
    /// Video bytes allowed to wait for all receivers of the node together.
    pub node_queue_bytes: usize,
    /// Video bytes allowed to wait for all receiving links of one remote address block (one
    /// IPv4 address or IPv6 /64, as for the link caps).
    pub source_queue_bytes: usize,
    /// How long a frame may wait in a queue, and then how long its send may take, before it is
    /// dropped. Matches the receiving side's `veil_net::frames::FRAME_DEADLINE`.
    pub send_deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rooms: 1_000,
            max_members_per_room: 16,
            queue_bytes: 32 * 1024 * 1024,
            node_queue_bytes: 1024 * 1024 * 1024,
            source_queue_bytes: 128 * 1024 * 1024,
            send_deadline: veil_net::frames::FRAME_DEADLINE,
        }
    }
}

/// Queued video bytes that are counted across receivers.
#[derive(Default)]
struct Budgets {
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
pub struct QueueMonitor(Arc<Budgets>);

impl QueueMonitor {
    /// Video bytes now queued for all receivers.
    pub fn queued_bytes(&self) -> usize {
        self.0.node.load(Ordering::Acquire)
    }
}

/// `len` bytes held against a receiver's, the node's and a source block's budgets. Dropping it
/// gives all three back.
struct Reservation {
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
fn reserve_all(
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

/// A frame waiting for one receiver. Its reservation is released when it is dropped.
struct Frame {
    id: u32,
    sealed: Arc<Vec<u8>>,
    enqueued: Instant,
    _reservation: Reservation,
}

struct Member {
    /// Tells this link apart from a later link of the same device.
    id: u64,
    video: mpsc::Sender<Frame>,
    queued: Arc<AtomicUsize>,
    /// The receiving link's address block.
    source: IpAddr,
    conn: quinn::Connection,
    control: mpsc::Sender<ServerMsg>,
    subs: Subscriptions,
}

/// (room tag, host key).
type RoomKey = ([u8; 32], PublicId);

struct Room {
    epoch: u64,
    members: HashMap<PublicId, Member>,
}

impl Room {
    /// Takes a newer ticket's roster and drops every member it no longer lists.
    fn advance(&mut self, ticket: &RosterTicket) {
        self.epoch = ticket.epoch;
        let roster: HashSet<PublicId> = ticket.members.iter().copied().collect();
        self.members.retain(|device, member| {
            let keep = roster.contains(device);
            if !keep {
                member.conn.close(quinn::VarInt::from_u32(1), b"not on the roster");
            }
            keep
        });
    }
}

#[derive(Default)]
struct State {
    rooms: HashMap<RoomKey, Room>,
    next_id: u64,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> Option<MutexGuard<'_, State>> {
    // A poisoned lock means a handler panicked; stop serving rather than trust the state.
    state.lock().ok()
}

pub struct ForwarderServer {
    node: Node,
    state: Shared,
    budgets: Arc<Budgets>,
    limits: Limits,
    only_host: Option<PublicId>,
}

impl ForwarderServer {
    pub fn new(node: Node, limits: Limits) -> Self {
        Self {
            node,
            state: Shared::default(),
            budgets: Arc::default(),
            limits,
            only_host: None,
        }
    }

    /// Serves only rooms whose tickets `host` signed. For a forwarder a participant runs for
    /// its own call.
    pub fn only_host(mut self, host: PublicId) -> Self {
        self.only_host = Some(host);
        self
    }

    pub fn id(&self) -> PublicId {
        self.node.id()
    }

    /// Video bytes now queued for all receivers. For monitoring and tests.
    pub fn queued_bytes(&self) -> usize {
        self.monitor().queued_bytes()
    }

    /// A handle that reads [`Self::queued_bytes`] after [`Self::run`] has taken the server.
    pub fn monitor(&self) -> QueueMonitor {
        QueueMonitor(self.budgets.clone())
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.node.local_addr()
    }

    /// Serves until the node is closed.
    pub async fn run(self) {
        while let Some(incoming) = self.node.accept().await {
            if let Ok(link) = incoming {
                let (state, budgets) = (self.state.clone(), self.budgets.clone());
                tokio::spawn(serve(link, state, budgets, self.limits, self.only_host));
            }
        }
    }
}

struct Joining {
    video: mpsc::Sender<Frame>,
    queued: Arc<AtomicUsize>,
    source: IpAddr,
    conn: quinn::Connection,
    control: mpsc::Sender<ServerMsg>,
}

/// Admits `me` under `ticket`. Returns the room's member count and this link's id.
fn join(
    state: &Shared,
    limits: &Limits,
    ticket: &RosterTicket,
    me: PublicId,
    joining: Joining,
) -> Result<(u16, u64), Refusal> {
    if !ticket.lists(&me) {
        return Err(Refusal::NotListed);
    }
    let mut s = lock(state).ok_or(Refusal::Full)?;
    let key = (ticket.room_tag, ticket.host);
    if !s.rooms.contains_key(&key) && s.rooms.len() >= limits.max_rooms {
        return Err(Refusal::Full);
    }
    s.next_id = s.next_id.wrapping_add(1);
    let id = s.next_id;
    let room = s.rooms.entry(key).or_insert_with(|| Room {
        epoch: ticket.epoch,
        members: HashMap::new(),
    });
    if ticket.epoch < room.epoch {
        return Err(Refusal::StaleTicket);
    }
    if ticket.epoch > room.epoch {
        room.advance(ticket);
    }
    if room.members.contains_key(&me) {
        return Err(Refusal::AlreadyJoined);
    }
    if room.members.len() >= limits.max_members_per_room {
        return Err(Refusal::RoomFull);
    }
    let Joining { video, queued, source, conn, control } = joining;
    let subs = Subscriptions::default();
    room.members.insert(me, Member { id, video, queued, source, conn, control, subs });
    Ok((u16::try_from(room.members.len()).unwrap_or(u16::MAX), id))
}

/// A member's newer ticket for its own room. Anything else is ignored.
fn update(state: &Shared, key: &RoomKey, sealed: &[u8]) {
    let Ok(ticket) = RosterTicket::open(sealed) else {
        return;
    };
    if (ticket.room_tag, ticket.host) != *key {
        return;
    }
    if let Some(mut s) = lock(state)
        && let Some(room) = s.rooms.get_mut(key)
        && ticket.epoch > room.epoch
    {
        room.advance(&ticket);
    }
}

/// A member's layer request for the senders it names. Senders whose raise waits for a
/// keyframe are asked for one.
fn subscribe(state: &Shared, key: &RoomKey, me: PublicId, id: u64, request: &[(PublicId, Wanted)]) {
    let raised = lock(state)
        .and_then(|mut s| {
            let member = s.rooms.get_mut(key)?.members.get_mut(&me)?;
            (member.id == id).then(|| member.subs.update(request))
        })
        .unwrap_or_default();
    for sender in raised {
        request_keyframe(state, key, me, id, sender);
    }
}

/// Passes a keyframe request from member `me` (this link `id`) to `sender` in the same room.
fn request_keyframe(state: &Shared, key: &RoomKey, me: PublicId, id: u64, sender: PublicId) {
    if sender == me {
        return;
    }
    if let Some(s) = lock(state)
        && let Some(room) = s.rooms.get(key)
        && room.members.get(&me).is_some_and(|m| m.id == id)
        && let Some(target) = room.members.get(&sender)
    {
        let _ = target.control.try_send(ServerMsg::KeyframeRequest);
    }
}

/// Passes a sealed app message from member `me` (this link `id`) to everyone else in the
/// room. Too large, or from a replaced link, it is dropped; a full control queue drops it
/// for that receiver only.
fn fan_out_app(state: &Shared, key: &RoomKey, me: PublicId, id: u64, sealed: Vec<u8>) {
    if sealed.len() > MAX_APP_LEN {
        return;
    }
    let Some(s) = lock(state) else { return };
    let Some(room) = s.rooms.get(key) else { return };
    if !room.members.get(&me).is_some_and(|m| m.id == id) {
        return;
    }
    for (_, member) in room.members.iter().filter(|(device, _)| **device != me) {
        let _ = member.control.try_send(ServerMsg::App(sealed.clone()));
    }
}

fn leave(state: &Shared, key: &RoomKey, me: PublicId, id: u64) {
    if let Some(mut s) = lock(state)
        && let Some(room) = s.rooms.get_mut(key)
    {
        if room.members.get(&me).is_some_and(|m| m.id == id) {
            room.members.remove(&me);
        }
        if room.members.is_empty() {
            s.rooms.remove(key);
        }
    }
}

/// Everyone in the room except the sender.
fn others<T>(state: &Shared, key: &RoomKey, me: PublicId, pick: impl Fn(&Member) -> T) -> Vec<T> {
    lock(state)
        .and_then(|s| {
            s.rooms.get(key).map(|room| {
                room.members
                    .iter()
                    .filter(|(device, _)| **device != me)
                    .map(|(_, m)| pick(m))
                    .collect()
            })
        })
        .unwrap_or_default()
}

async fn refuse(link: &Link, out: &mut FramedSend, refusal: Refusal) {
    let _ = out.send(&ServerMsg::Refused(refusal)).await;
    // Closing at once can discard the refusal in flight; let the member hang up first.
    let _ = tokio::time::timeout(REFUSAL_LINGER, link.quic().closed()).await;
}

async fn serve(link: Link, state: Shared, budgets: Arc<Budgets>, limits: Limits, only_host: Option<PublicId>) {
    let link = Arc::new(link);
    // A link that does not join promptly is dropped, so idle links cannot pile up.
    let joined = tokio::time::timeout(JOIN_WAIT, async {
        let (out, mut input) = link.accept_control().await.ok()?.split();
        match input.recv::<ClientMsg>().await.ok()? {
            ClientMsg::Join { ticket } => Some((out, input, ticket)),
            _ => None,
        }
    })
    .await;
    let Ok(Some((mut out, mut input, ticket))) = joined else {
        return;
    };
    let Ok(ticket) = RosterTicket::open(&ticket) else {
        refuse(&link, &mut out, Refusal::BadTicket).await;
        return;
    };
    if only_host.is_some_and(|host| host != ticket.host) {
        refuse(&link, &mut out, Refusal::NotServed).await;
        return;
    }
    let key = (ticket.room_tag, ticket.host);
    let me = link.peer();
    let (video_tx, mut video_rx) = mpsc::channel::<Frame>(VIDEO_QUEUE);
    let queued = Arc::new(AtomicUsize::new(0));
    let (control_tx, mut control_rx) = mpsc::channel::<ServerMsg>(CONTROL_QUEUE);
    let joining = Joining {
        video: video_tx,
        queued: queued.clone(),
        source: block(link.remote_addr().ip()),
        conn: link.quic().clone(),
        control: control_tx,
    };
    let id = match join(&state, &limits, &ticket, me, joining) {
        Ok((members, id)) => {
            if out.send(&ServerMsg::Joined { members }).await.is_err() {
                leave(&state, &key, me, id);
                return;
            }
            id
        }
        Err(refusal) => {
            refuse(&link, &mut out, refusal).await;
            return;
        }
    };

    let control_writer = tokio::spawn(async move {
        while let Some(message) = control_rx.recv().await {
            if out.send(&message).await.is_err() {
                break;
            }
        }
    });
    let writer = {
        let link = link.clone();
        tokio::spawn(async move {
            while let Some(frame) = video_rx.recv().await {
                // A frame that waited too long is stale: drop it (its reservation goes with it).
                if frame.enqueued.elapsed() >= limits.send_deadline {
                    continue;
                }
                // On timeout the send future is dropped; `send_frame` resets the frame's QUIC
                // stream then, discarding its buffered bytes. The receiver sees one lost frame
                // and later frames are unaffected, because each frame has a stream of its own.
                match tokio::time::timeout(limits.send_deadline, link.send_frame(frame.id, &frame.sealed)).await {
                    Ok(Ok(())) | Err(_) => {}
                    Ok(Err(_)) => break,
                }
            }
        })
    };
    let video = {
        let (link, state, budgets) = (link.clone(), state.clone(), budgets.clone());
        tokio::spawn(async move {
            let mut frames = veil_net::FrameReader::new(&link);
            while let Ok((frame_id, sealed)) = frames.recv().await {
                fan_out_video(&state, &budgets, &limits, &key, me, frame_id, sealed);
            }
        })
    };
    let audio = {
        let (conn, state) = (link.quic().clone(), state.clone());
        tokio::spawn(async move {
            while let Ok(packet) = conn.read_datagram().await {
                fan_out_audio(&state, &key, me, &packet);
            }
        })
    };

    // Reading control messages also detects the link closing.
    while let Ok(message) = input.recv::<ClientMsg>().await {
        match message {
            ClientMsg::Ticket(sealed) => update(&state, &key, &sealed),
            ClientMsg::Subscribe(request) => subscribe(&state, &key, me, id, &request),
            ClientMsg::KeyframeRequest(sender) => request_keyframe(&state, &key, me, id, sender),
            ClientMsg::App(sealed) => fan_out_app(&state, &key, me, id, sealed),
            ClientMsg::Join { .. } => {}
        }
    }

    leave(&state, &key, me, id);
    control_writer.abort();
    writer.abort();
    video.abort();
    audio.abort();
}

fn fan_out_video(
    state: &Shared,
    budgets: &Arc<Budgets>,
    limits: &Limits,
    key: &RoomKey,
    me: PublicId,
    frame_id: u32,
    sealed: Vec<u8>,
) {
    let Ok(layer) = frame_layer(&sealed) else {
        return;
    };
    let receivers: Vec<_> = lock(state)
        .and_then(|mut s| {
            s.rooms.get_mut(key).map(|room| {
                room.members
                    .iter_mut()
                    .filter(|(device, _)| **device != me)
                    .filter_map(|(_, m)| m.subs.pass(&me, layer).then(|| (m.video.clone(), m.queued.clone(), m.source)))
                    .collect()
            })
        })
        .unwrap_or_default();
    let len = sealed.len();
    let frame = Arc::new(sealed);
    for (queue, queued, source) in receivers {
        if let Some(reservation) = reserve_all(budgets, &queued, source, len, limits) {
            // A full queue hands the frame back; dropping it releases the reservation.
            let _ = queue.try_send(Frame {
                id: frame_id,
                sealed: frame.clone(),
                enqueued: Instant::now(),
                _reservation: reservation,
            });
        }
    }
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

fn fan_out_audio(state: &Shared, key: &RoomKey, me: PublicId, packet: &Bytes) {
    for conn in others(state, key, me, |m| m.conn.clone()) {
        // Audio is loss tolerant: a full or too-small path drops the packet.
        let _ = conn.send_datagram(packet.clone());
    }
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
