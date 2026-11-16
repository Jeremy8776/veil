//! The forwarding service.
//!
//! State lives behind one mutex that is never held across an `.await`. Each member link has a
//! control reader (which also detects the link closing), a video reader, an audio reader and a
//! video writer. Video for a member waits in a queue bounded in bytes; when the queue is full,
//! new frames for that member are dropped.
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
//! shortcut: no forwarder-to-forwarder cascade yet; one forwarder serves a whole room.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use tokio::sync::mpsc;
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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rooms: 1_000,
            max_members_per_room: 16,
            queue_bytes: 32 * 1024 * 1024,
        }
    }
}

type Frame = (u32, Arc<Vec<u8>>);

struct Member {
    /// Tells this link apart from a later link of the same device.
    id: u64,
    video: mpsc::Sender<Frame>,
    queued: Arc<AtomicUsize>,
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
    limits: Limits,
}

impl ForwarderServer {
    pub fn new(node: Node, limits: Limits) -> Self {
        Self {
            node,
            state: Shared::default(),
            limits,
        }
    }

    pub fn id(&self) -> PublicId {
        self.node.id()
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.node.local_addr()
    }

    /// Serves until the node is closed.
    pub async fn run(self) {
        while let Some(incoming) = self.node.accept().await {
            if let Ok(link) = incoming {
                tokio::spawn(serve(link, self.state.clone(), self.limits));
            }
        }
    }
}

struct Joining {
    video: mpsc::Sender<Frame>,
    queued: Arc<AtomicUsize>,
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
    let Joining { video, queued, conn, control } = joining;
    let subs = Subscriptions::default();
    room.members.insert(me, Member { id, video, queued, conn, control, subs });
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

async fn serve(link: Link, state: Shared, limits: Limits) {
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
    let key = (ticket.room_tag, ticket.host);
    let me = link.peer();
    let (video_tx, mut video_rx) = mpsc::channel::<Frame>(VIDEO_QUEUE);
    let queued = Arc::new(AtomicUsize::new(0));
    let (control_tx, mut control_rx) = mpsc::channel::<ServerMsg>(CONTROL_QUEUE);
    let joining = Joining {
        video: video_tx,
        queued: queued.clone(),
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
            while let Some((frame_id, frame)) = video_rx.recv().await {
                let sent = link.send_frame(frame_id, &frame).await;
                release(&queued, frame.len());
                if sent.is_err() {
                    break;
                }
            }
        })
    };
    let video = {
        let (link, state) = (link.clone(), state.clone());
        tokio::spawn(async move {
            while let Ok((frame_id, sealed)) = link.recv_frame().await {
                fan_out_video(&state, &key, me, frame_id, sealed, limits.queue_bytes);
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

fn fan_out_video(state: &Shared, key: &RoomKey, me: PublicId, frame_id: u32, sealed: Vec<u8>, budget: usize) {
    let Ok(layer) = frame_layer(&sealed) else {
        return;
    };
    let receivers: Vec<_> = lock(state)
        .and_then(|mut s| {
            s.rooms.get_mut(key).map(|room| {
                room.members
                    .iter_mut()
                    .filter(|(device, _)| **device != me)
                    .filter_map(|(_, m)| m.subs.pass(&me, layer).then(|| (m.video.clone(), m.queued.clone())))
                    .collect()
            })
        })
        .unwrap_or_default();
    let len = sealed.len();
    let frame = Arc::new(sealed);
    for (queue, queued) in receivers {
        if reserve(&queued, len, budget) && queue.try_send((frame_id, frame.clone())).is_err() {
            release(&queued, len);
        }
    }
}

/// Reserves `len` bytes of a receiver's budget. Atomic, so concurrent senders in one room
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
}
