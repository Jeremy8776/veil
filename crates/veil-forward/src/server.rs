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
//! Room slots: a new room counts against the address block of the link that opened it, up to
//! [`Limits::max_rooms_per_block`], until it closes. A member alone in its room for
//! [`Limits::alone_grace`] has its link ended, since a room of one forwards nothing. So one
//! party cannot hold every room slot with idle links (GHSA-rgc8-82r3-49vh).
//!
//! Serving one host: a participant's own forwarder runs for its own call only
//! ([`ForwarderServer::only_host`]). It refuses tickets signed by any other key, so strangers
//! who find its port cannot use the participant's upload for their own rooms.
//!
//! shortcut: no forwarder-to-forwarder cascade yet; one forwarder serves a whole room.

mod budget;
mod fan_out;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use veil_net::accept::block;
use veil_net::{FramedSend, Link, NetError, Node};
use veil_security::PublicId;

use crate::protocol::{ClientMsg, Refusal, ServerMsg};
use crate::subscribe::{Subscriptions, Wanted};
use crate::ticket::RosterTicket;
pub use budget::QueueMonitor;
use budget::{Budgets, Reservation};
use fan_out::{fan_out_app, fan_out_audio, fan_out_video};

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
    /// Rooms that links from one remote address block (one IPv4 address or IPv6 /64) may have
    /// opened and still hold, so one party cannot take every room slot.
    pub max_rooms_per_block: usize,
    /// How long a room may have a single member before that member's link is ended. A room of
    /// one forwards nothing; without this, one link per room would hold slots for free.
    pub alone_grace: Duration,
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
            // A room that lasts needs two links (`alone_grace`), so the link cap binds before
            // the room cap does: room slots cannot run out while links are left.
            max_rooms: veil_net::accept::MAX_LINKS / 2,
            // Several calls from one home or office router; one party needs many blocks.
            max_rooms_per_block: 16,
            alone_grace: Duration::from_secs(60),
            max_members_per_room: 16,
            queue_bytes: 32 * 1024 * 1024,
            node_queue_bytes: 1024 * 1024 * 1024,
            source_queue_bytes: 128 * 1024 * 1024,
            send_deadline: veil_net::frames::FRAME_DEADLINE,
        }
    }
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
    /// The address block of the link that opened the room; the room counts against it.
    owner: IpAddr,
    /// Since when the room has had at most one member, if it has.
    alone_since: Option<Instant>,
}

impl Room {
    /// Takes a newer ticket's roster and drops every member it no longer lists.
    fn advance(&mut self, ticket: &RosterTicket) {
        self.epoch = ticket.epoch;
        let roster: HashSet<PublicId> = ticket.members.iter().copied().collect();
        self.members.retain(|device, member| {
            let keep = roster.contains(device);
            if !keep {
                member
                    .conn
                    .close(quinn::VarInt::from_u32(1), b"not on the roster");
            }
            keep
        });
        self.recount();
    }

    /// Starts or clears the alone clock after the member count changed.
    fn recount(&mut self) {
        if self.members.len() > 1 {
            self.alone_since = None;
        } else if self.alone_since.is_none() {
            self.alone_since = Some(Instant::now());
        }
    }
}

#[derive(Default)]
struct State {
    rooms: HashMap<RoomKey, Room>,
    /// Open rooms per owner address block. An entry is removed when it drops to zero.
    blocks: HashMap<IpAddr, usize>,
    next_id: u64,
}

impl State {
    fn remove_room(&mut self, key: &RoomKey) {
        let Some(room) = self.rooms.remove(key) else {
            return;
        };
        if let Some(count) = self.blocks.get_mut(&room.owner) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.blocks.remove(&room.owner);
            }
        }
    }
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
    /// When set, the only hosts whose rooms this forwarder serves.
    hosts: Option<Arc<HashSet<PublicId>>>,
}

impl ForwarderServer {
    pub fn new(node: Node, limits: Limits) -> Self {
        Self {
            node,
            state: Shared::default(),
            budgets: Arc::default(),
            limits,
            hosts: None,
        }
    }

    /// Serves only rooms whose tickets `host` signed. For a forwarder a participant runs for
    /// its own call.
    pub fn only_host(self, host: PublicId) -> Self {
        self.only_hosts([host])
    }

    /// Serves only rooms whose tickets one of `hosts` signed. For a node one person runs for
    /// themselves and the people they choose.
    pub fn only_hosts(mut self, hosts: impl IntoIterator<Item = PublicId>) -> Self {
        self.hosts = Some(Arc::new(hosts.into_iter().collect()));
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
        let mut members = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming = self.node.accept() => match incoming {
                    Some(Ok(link)) => {
                        let (state, budgets) = (self.state.clone(), self.budgets.clone());
                        members.spawn(serve(link, state, budgets, self.limits, self.hosts.clone()));
                    }
                    Some(Err(_)) => {}
                    None => break,
                },
                _ = members.join_next(), if !members.is_empty() => {}
            }
        }
    }
}

impl Drop for ForwarderServer {
    fn drop(&mut self) {
        self.node.close();
    }
}

/// Cancellation of a member handler must also stop its media/control workers.
struct LinkTasks(Vec<tokio::task::AbortHandle>);
impl Drop for LinkTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
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
    if !s.rooms.contains_key(&key) {
        // A new room counts against the block of the link that opens it (GHSA-rgc8-82r3-49vh).
        let owned = s.blocks.get(&joining.source).copied().unwrap_or(0);
        if s.rooms.len() >= limits.max_rooms || owned >= limits.max_rooms_per_block {
            return Err(Refusal::Full);
        }
        *s.blocks.entry(joining.source).or_default() += 1;
        let room = Room {
            epoch: ticket.epoch,
            members: HashMap::new(),
            owner: joining.source,
            alone_since: Some(Instant::now()),
        };
        s.rooms.insert(key, room);
    }
    s.next_id = s.next_id.wrapping_add(1);
    let id = s.next_id;
    let Some(room) = s.rooms.get_mut(&key) else {
        return Err(Refusal::Full);
    };
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
    let Joining {
        video,
        queued,
        source,
        conn,
        control,
    } = joining;
    let subs = Subscriptions::default();
    room.members.insert(
        me,
        Member {
            id,
            video,
            queued,
            source,
            conn,
            control,
            subs,
        },
    );
    room.recount();
    Ok((u16::try_from(room.members.len()).unwrap_or(u16::MAX), id))
}

/// A non-mutating admission snapshot. In particular a probe with a future epoch must not
/// drop the old roster or take a slot from an active call while its replacement is tested.
fn check_available(
    state: &Shared,
    limits: &Limits,
    ticket: &RosterTicket,
    me: PublicId,
    source: IpAddr,
) -> Result<(), Refusal> {
    if !ticket.lists(&me) {
        return Err(Refusal::NotListed);
    }
    if ticket.members.len() > limits.max_members_per_room {
        return Err(Refusal::RoomFull);
    }
    let s = lock(state).ok_or(Refusal::Full)?;
    match s.rooms.get(&(ticket.room_tag, ticket.host)) {
        Some(room) => {
            if ticket.epoch < room.epoch {
                return Err(Refusal::StaleTicket);
            }
            if !room.members.contains_key(&me) && room.members.len() >= limits.max_members_per_room
            {
                return Err(Refusal::RoomFull);
            }
        }
        None => {
            let owned = s.blocks.get(&source).copied().unwrap_or(0);
            if s.rooms.len() >= limits.max_rooms || owned >= limits.max_rooms_per_block {
                return Err(Refusal::Full);
            }
        }
    }
    Ok(())
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

fn leave(state: &Shared, key: &RoomKey, me: PublicId, id: u64) {
    if let Some(mut s) = lock(state)
        && let Some(room) = s.rooms.get_mut(key)
    {
        if room.members.get(&me).is_some_and(|m| m.id == id) {
            room.members.remove(&me);
            room.recount();
        }
        if room.members.is_empty() {
            s.remove_room(key);
        }
    }
}

/// Whether member `me` (this link `id`) has been alone in its room for `grace`. `None` once the
/// member has left, so the watcher can stop.
fn alone_too_long(
    state: &Shared,
    key: &RoomKey,
    me: PublicId,
    id: u64,
    grace: Duration,
) -> Option<bool> {
    let s = lock(state)?;
    let room = s.rooms.get(key)?;
    room.members.get(&me).filter(|m| m.id == id)?;
    Some(
        room.alone_since
            .is_some_and(|since| since.elapsed() >= grace),
    )
}

/// Ends this member's link once it has been alone in its room for `limits.alone_grace`.
async fn watch_alone(
    link: Arc<Link>,
    state: Shared,
    key: RoomKey,
    me: PublicId,
    id: u64,
    grace: Duration,
) {
    let step = (grace / 4).max(Duration::from_millis(50));
    loop {
        tokio::time::sleep(step).await;
        match alone_too_long(&state, &key, me, id, grace) {
            Some(true) => {
                link.close("alone in the room");
                return;
            }
            Some(false) => {}
            None => return,
        }
    }
}

async fn refuse(link: &Link, out: &mut FramedSend, refusal: Refusal) {
    let _ = out.send(&ServerMsg::Refused(refusal)).await;
    // Closing at once can discard the refusal in flight; let the member hang up first.
    let _ = tokio::time::timeout(REFUSAL_LINGER, link.quic().closed()).await;
}

async fn serve(
    link: Link,
    state: Shared,
    budgets: Arc<Budgets>,
    limits: Limits,
    hosts: Option<Arc<HashSet<PublicId>>>,
) {
    let link = Arc::new(link);
    // A link that does not join promptly is dropped, so idle links cannot pile up.
    let joined = tokio::time::timeout(JOIN_WAIT, async {
        let (out, mut input) = link.accept_control().await.ok()?.split();
        match input.recv::<ClientMsg>().await.ok()? {
            ClientMsg::Join { ticket } => Some((out, input, ticket, false)),
            ClientMsg::Probe { ticket } => Some((out, input, ticket, true)),
            _ => None,
        }
    })
    .await;
    let Ok(Some((mut out, mut input, ticket, probe))) = joined else {
        return;
    };
    let Ok(ticket) = RosterTicket::open(&ticket) else {
        refuse(&link, &mut out, Refusal::BadTicket).await;
        return;
    };
    if hosts.is_some_and(|hosts| !hosts.contains(&ticket.host)) {
        refuse(&link, &mut out, Refusal::NotServed).await;
        return;
    }
    let key = (ticket.room_tag, ticket.host);
    let me = link.peer();
    if probe {
        let checked = check_available(&state, &limits, &ticket, me, block(link.remote_addr().ip()));
        match checked {
            Ok(()) => {
                // Keep the response alive until its receiver closes the link. This branch
                // never registers a member and never advances an existing room's epoch.
                let _ = tokio::time::timeout(JOIN_WAIT, out.send(&ServerMsg::Available)).await;
                let _ = tokio::time::timeout(REFUSAL_LINGER, link.quic().closed()).await;
            }
            Err(why) => refuse(&link, &mut out, why).await,
        }
        return;
    }
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
                match tokio::time::timeout(
                    limits.send_deadline,
                    link.send_frame(frame.id, &frame.sealed),
                )
                .await
                {
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
    let alone = tokio::spawn(watch_alone(
        link.clone(),
        state.clone(),
        key,
        me,
        id,
        limits.alone_grace,
    ));
    let _workers = LinkTasks(vec![
        control_writer.abort_handle(),
        writer.abort_handle(),
        video.abort_handle(),
        audio.abort_handle(),
        alone.abort_handle(),
    ]);

    // Reading control messages also detects the link closing.
    while let Ok(message) = input.recv::<ClientMsg>().await {
        match message {
            ClientMsg::Ticket(sealed) => update(&state, &key, &sealed),
            ClientMsg::Subscribe(request) => subscribe(&state, &key, me, id, &request),
            ClientMsg::KeyframeRequest(sender) => request_keyframe(&state, &key, me, id, sender),
            ClientMsg::App(sealed) => fan_out_app(&state, &key, me, id, sealed),
            ClientMsg::Join { .. } | ClientMsg::Probe { .. } => {}
        }
    }

    leave(&state, &key, me, id);
    control_writer.abort();
    writer.abort();
    video.abort();
    audio.abort();
    alone.abort();
}
