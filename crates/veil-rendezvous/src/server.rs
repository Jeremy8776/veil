//! The rendezvous and relay service. Runs on any Veil node.
//!
//! State lives behind one mutex that is never held across an `.await`. Each member link has
//! a reader loop (requests), a writer task (replies and notices) and two relay forwarders
//! (datagrams and the stream fallback).
//!
//! Consent: an introduction or relay session is only a request until its target accepts
//! (`ClientMsg::Accept`). Until then the asker learns nothing about the target, not even
//! that it is online, so holding an invite is not enough to learn members' IP addresses or
//! to fill their relay slots. Requests expire after `REQUEST_TTL`; each member may have
//! `MAX_REQUESTS_PER_MEMBER` open and hold `MAX_SESSIONS_PER_MEMBER` relay sessions.
//! A link must open its control stream within `CONTROL_WAIT`, or it is closed.
//!
//! Admission: a room is named by its tag and its host's key, and only the host may open it.
//! Everyone else must be on the host's allow list (`ClientMsg::Allow`: members, and joiners
//! waiting in its lobby) or show one of the lobby passes the host opened, one key per pass.
//! When the host's lists change, anyone they no longer cover is dropped from the room, with
//! their relay sessions and requests. So a person the host declined or removed cannot come
//! back with throwaway keys and fill the room.
//!
//! Serving one call: a participant's own server serves its own room only
//! ([`RendezvousServer::only_room`]), so strangers cannot use its upload as a relay.
//! Bandwidth: each session has its own cap and the server has one for all of them together
//! (`limit.rs`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, mpsc};
use veil_net::{Link, NetError, Node};
use veil_security::PublicId;

use crate::invites::{MAX_FETCHES_PER_LINK, Shelf};
use crate::limit::{Bucket, TOTAL_BURST, TOTAL_RATE};
use crate::reach::{self, LinkChecks};
use crate::room::{Member, Room, RoomKey};
use crate::protocol::{
    Ask, ClientMsg, MAX_ADDRS, MAX_PASSES, Refusal, STREAM_HEADER, ServerMsg,
    parse_stream_header, split_session,
};

pub const MAX_ROOMS: usize = 10_000;
pub const MAX_MEMBERS_PER_ROOM: usize = 64;
pub const MAX_SESSIONS: usize = 4_096;
/// Relay sessions one member may hold. A call has at most 16 members.
pub const MAX_SESSIONS_PER_MEMBER: usize = 16;
/// Requests one member may have waiting for an answer.
pub const MAX_REQUESTS_PER_MEMBER: usize = 16;
/// How long a request waits for its target to accept.
pub const REQUEST_TTL: Duration = Duration::from_secs(10);
const OUTBOX: usize = 64;
/// A link that opens no control stream in this time is dropped.
const CONTROL_WAIT: Duration = Duration::from_secs(10);

/// A relay session between exactly two members. Generic so routing can be unit tested.
#[derive(Clone)]
struct Session<T> {
    a: (PublicId, T),
    b: (PublicId, T),
}

impl<T> Session<T> {
    /// Where a packet from `from` goes. `None` if `from` is not in this session.
    fn other(&self, from: PublicId) -> Option<&T> {
        if from == self.a.0 {
            Some(&self.b.1)
        } else if from == self.b.0 {
            Some(&self.a.1)
        } else {
            None
        }
    }

    fn involves(&self, key: PublicId) -> bool {
        self.a.0 == key || self.b.0 == key
    }
}

struct State {
    rooms: HashMap<RoomKey, Room>,
    sessions: HashMap<u32, Session<quinn::Connection>>,
    /// Requests waiting for their target: (from, to, kind) to when asked.
    requests: HashMap<(PublicId, PublicId, Ask), Instant>,
    next_session: u32,
    relay_disabled: bool,
    /// When set, the only room this server serves.
    only_room: Option<RoomKey>,
    /// Every relayed byte, all sessions together.
    total: Bucket,
    /// Sealed invites for short links (`invites.rs`).
    invites: Shelf,
}

impl Default for State {
    fn default() -> Self {
        Self {
            rooms: HashMap::new(),
            sessions: HashMap::new(),
            requests: HashMap::new(),
            next_session: 0,
            relay_disabled: false,
            only_room: None,
            total: Bucket::with_rate(TOTAL_RATE, TOTAL_BURST, Instant::now()),
            invites: Shelf::default(),
        }
    }
}

impl State {
    /// Drops `key`'s relay sessions and requests.
    fn forget(&mut self, key: PublicId) {
        self.sessions.retain(|_, session| !session.involves(key));
        self.requests.retain(|(from, to, _), _| *from != key && *to != key);
    }
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> Option<MutexGuard<'_, State>> {
    // A poisoned lock means a handler panicked; stop serving rather than trust the state.
    state.lock().ok()
}

pub struct RendezvousServer {
    node: Node,
    state: Shared,
    /// Reachability checks running, all links together (`reach.rs`).
    checks: Arc<Semaphore>,
}

impl RendezvousServer {
    pub fn new(node: Node) -> Self {
        Self {
            node,
            state: Shared::default(),
            checks: Arc::new(Semaphore::new(reach::MAX_RUNNING)),
        }
    }

    /// Turns the relay role off: reflection and introductions still work, relay requests
    /// are refused with [`Refusal::RelayDisabled`].
    pub fn without_relay(self) -> Self {
        if let Some(mut s) = lock(&self.state) {
            s.relay_disabled = true;
        }
        self
    }

    /// Serves only the room `room_tag` hosted by `host`; any other registration is refused
    /// with [`Refusal::UnknownRoom`]. For a server a participant runs for its own call.
    pub fn only_room(self, room_tag: [u8; 32], host: PublicId) -> Self {
        if let Some(mut s) = lock(&self.state) {
            s.only_room = Some((room_tag, host));
        }
        self
    }

    /// Caps all relayed bytes together at `bytes_per_second` (default
    /// [`crate::limit::TOTAL_RATE`]), with a tenth of a second of burst.
    pub fn with_total_rate(self, bytes_per_second: f64) -> Self {
        if let Some(mut s) = lock(&self.state) {
            s.total = Bucket::with_rate(bytes_per_second, bytes_per_second / 10.0, Instant::now());
        }
        self
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
                tokio::spawn(serve(link, self.state.clone(), self.checks.clone()));
            }
        }
    }
}

async fn serve(link: Link, state: Shared, checks: Arc<Semaphore>) {
    let Ok(Ok(control)) = tokio::time::timeout(CONTROL_WAIT, link.accept_control()).await else {
        return;
    };
    let (mut out, mut input) = control.split();
    let (outbox, mut rx) = mpsc::channel::<ServerMsg>(OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if out.send(&message).await.is_err() {
                break;
            }
        }
    });
    let me = link.peer();
    let conn = link.quic().clone();
    let buckets = Buckets::default();
    let datagrams = tokio::spawn(forward_datagrams(conn.clone(), me, state.clone(), buckets.clone()));
    let streams = tokio::spawn(forward_streams(conn, me, state.clone(), buckets));

    let mut room: Option<RoomKey> = None;
    let mut mine = LinkChecks::default();
    let mut fetches = 0;
    while let Ok(message) = input.recv::<ClientMsg>().await {
        // Needs no room and no shared state, so it runs apart from `handle`.
        if let ClientMsg::CheckReach { port } = message {
            mine.start(&checks, &link, port, &outbox);
            continue;
        }
        if matches!(message, ClientMsg::GetInvite { .. }) {
            fetches += 1;
            if fetches > MAX_FETCHES_PER_LINK {
                let _ = outbox.try_send(ServerMsg::Refused(Refusal::Full));
                continue;
            }
        }
        let replies = handle(message, &link, &mut room, &outbox, &state);
        for (to, reply) in replies {
            // A member that stops reading loses notices; it does not stall the server.
            let _ = to.try_send(reply);
        }
    }

    // Only the link that registered holds the member's place. The same key's other links (a
    // reachability check, storing an invite) must not take its relay sessions with them.
    if let Some(key) = room
        && let Some(mut s) = lock(&state)
    {
        if let Some(r) = s.rooms.get_mut(&key) {
            r.members.remove(&me);
            if r.members.is_empty() {
                s.rooms.remove(&key);
            }
        }
        if key.1 == me {
            s.invites.drop_room(&key);
        }
        s.forget(me);
    }
    writer.abort();
    datagrams.abort();
    streams.abort();
}

type Outgoing = Vec<(mpsc::Sender<ServerMsg>, ServerMsg)>;

fn handle(
    message: ClientMsg,
    link: &Link,
    room: &mut Option<RoomKey>,
    outbox: &mpsc::Sender<ServerMsg>,
    state: &Shared,
) -> Outgoing {
    let me = link.peer();
    let refuse = |r| vec![(outbox.clone(), ServerMsg::Refused(r))];
    let Some(mut s) = lock(state) else {
        return refuse(Refusal::Full);
    };

    match message {
        ClientMsg::Register {
            room_tag,
            host,
            pass,
            local_addrs,
        } => {
            if room.is_some() {
                return refuse(Refusal::AlreadyRegistered);
            }
            if local_addrs.len() > MAX_ADDRS {
                return refuse(Refusal::TooManyAddrs);
            }
            let key = (room_tag, host);
            if s.only_room.is_some_and(|only| only != key) {
                return refuse(Refusal::UnknownRoom);
            }
            let is_host = me == host;
            if !s.rooms.contains_key(&key) {
                // Only the host opens its room.
                if !is_host {
                    return refuse(Refusal::UnknownRoom);
                }
                if s.rooms.len() >= MAX_ROOMS {
                    return refuse(Refusal::Full);
                }
                s.rooms.insert(key, Room::new());
            }
            let Some(r) = s.rooms.get_mut(&key) else {
                return refuse(Refusal::UnknownRoom);
            };
            if r.members.contains_key(&me) {
                return refuse(Refusal::AlreadyRegistered);
            }
            // A key on the allow list needs no pass, and keeps none it might have shown.
            let pass = pass.filter(|_| !is_host && !r.allowed.contains(&me));
            if !r.entitled(host, me, pass.as_ref()) || pass.as_ref().is_some_and(|p| r.pass_taken(p)) {
                return refuse(Refusal::NotAllowed);
            }
            if r.members.len() >= MAX_MEMBERS_PER_ROOM {
                return refuse(Refusal::Full);
            }
            let observed = link.remote_addr();
            let mut addrs = vec![observed];
            addrs.extend(local_addrs.into_iter().filter(|a| *a != observed));
            r.members.insert(
                me,
                Member {
                    addrs,
                    outbox: outbox.clone(),
                    conn: link.quic().clone(),
                    pass,
                },
            );
            *room = Some(key);
            vec![(outbox.clone(), ServerMsg::Registered { observed })]
        }

        ClientMsg::Allow { members, passes } => allow(&mut s, room, me, members, passes, outbox),

        ClientMsg::Introduce { peer } => ask(&mut s, room, me, peer, Ask::Introduce, outbox),
        ClientMsg::OpenRelay { peer } => ask(&mut s, room, me, peer, Ask::Relay, outbox),
        ClientMsg::Accept { from, kind } => accept(&mut s, room, from, me, kind),
        // Taken by `serve` before it gets here.
        ClientMsg::CheckReach { .. } => Vec::new(),

        ClientMsg::PutInvite { room_tag, id, sealed } => {
            // The key is part of the room's name, so this finds only a room `me` hosts.
            let key = (room_tag, me);
            if !s.rooms.contains_key(&key) {
                return refuse(Refusal::NotAllowed);
            }
            if !s.invites.put(key, id, sealed) {
                return refuse(Refusal::Full);
            }
            vec![(outbox.clone(), ServerMsg::InviteStored)]
        }
        ClientMsg::GetInvite { id } => {
            let sealed = s.invites.get(&id);
            vec![(outbox.clone(), ServerMsg::SealedInvite { sealed })]
        }
    }
}

/// The host replaces its room's allow list and open passes. Members they no longer cover are
/// dropped from the room, with their sessions and requests.
fn allow(
    s: &mut State,
    room: &Option<RoomKey>,
    me: PublicId,
    members: Vec<PublicId>,
    passes: Vec<[u8; 32]>,
    outbox: &mpsc::Sender<ServerMsg>,
) -> Outgoing {
    let refuse = |r| vec![(outbox.clone(), ServerMsg::Refused(r))];
    let Some(key) = room.filter(|(_, host)| *host == me) else {
        return refuse(Refusal::NotAllowed);
    };
    if members.len() > MAX_MEMBERS_PER_ROOM || passes.len() > MAX_PASSES {
        return refuse(Refusal::Full);
    }
    let Some(r) = s.rooms.get_mut(&key) else {
        return Vec::new();
    };
    for device in r.allow(key.1, members, passes) {
        s.forget(device);
    }
    vec![(outbox.clone(), ServerMsg::Allowed)]
}

/// Records `me`'s request and asks `peer`. Nothing about `peer` reaches `me` yet.
fn ask(
    s: &mut State,
    room: &Option<RoomKey>,
    me: PublicId,
    peer: PublicId,
    kind: Ask,
    outbox: &mpsc::Sender<ServerMsg>,
) -> Outgoing {
    let refuse = |r| vec![(outbox.clone(), ServerMsg::Refused(r))];
    if kind == Ask::Relay && s.relay_disabled {
        return refuse(Refusal::RelayDisabled);
    }
    let Some(r) = room.and_then(|key| s.rooms.get(&key)).filter(|r| r.members.contains_key(&me))
    else {
        return refuse(Refusal::NotRegistered);
    };
    if peer == me {
        return refuse(Refusal::PeerNotInRoom);
    }
    let their_box = r.members.get(&peer).map(|theirs| theirs.outbox.clone());
    // Every check below depends on the asker alone, and a request to an absent key is
    // recorded like any other, so no reply or limit tells the asker whether `peer` is here.
    let now = Instant::now();
    s.requests.retain(|_, at| now.duration_since(*at) < REQUEST_TTL);
    let mine = s.requests.keys().filter(|(from, ..)| *from == me).count();
    if mine >= MAX_REQUESTS_PER_MEMBER {
        return refuse(Refusal::Full);
    }
    if kind == Ask::Relay && sessions_of(s, me) >= MAX_SESSIONS_PER_MEMBER {
        return refuse(Refusal::Full);
    }
    s.requests.insert((me, peer, kind), now);
    // An absent key gets no request and the asker no answer: the same as a member that
    // declines by staying silent.
    their_box.map_or_else(Vec::new, |to| vec![(to, ServerMsg::Request { from: me, kind })])
}

/// `me` agrees to `from`'s request: now the introduction or relay session goes ahead.
/// A request that is unknown, expired, or whose members left is ignored without a reply,
/// so a stray answer cannot fail an unrelated wait on the client.
fn accept(
    s: &mut State,
    room: &Option<RoomKey>,
    from: PublicId,
    me: PublicId,
    kind: Ask,
) -> Outgoing {
    let fresh = s
        .requests
        .remove(&(from, me, kind))
        .is_some_and(|at| at.elapsed() < REQUEST_TTL);
    let Some(key) = room.filter(|_| fresh) else {
        return Vec::new();
    };
    let Some(members) = s.rooms.get(&key).map(|r| &r.members) else {
        return Vec::new();
    };
    let (Some(asker), Some(mine)) = (members.get(&from), members.get(&me)) else {
        return Vec::new();
    };
    match kind {
        Ask::Introduce => {
            // Fire together: the nearer member waits half the round-trip difference.
            let (asker_rtt, my_rtt) = (asker.conn.rtt(), mine.conn.rtt());
            let longest = asker_rtt.max(my_rtt);
            let wait = |rtt: Duration| {
                u32::try_from((longest.saturating_sub(rtt) / 2).as_millis()).unwrap_or(u32::MAX)
            };
            vec![
                (
                    asker.outbox.clone(),
                    ServerMsg::Punch {
                        peer: me,
                        addrs: mine.addrs.clone(),
                        delay_ms: wait(asker_rtt),
                    },
                ),
                (
                    mine.outbox.clone(),
                    ServerMsg::Punch {
                        peer: from,
                        addrs: asker.addrs.clone(),
                        delay_ms: wait(my_rtt),
                    },
                ),
            ]
        }
        Ask::Relay => {
            let (asker_conn, asker_box) = (asker.conn.clone(), asker.outbox.clone());
            let (my_conn, my_box) = (mine.conn.clone(), mine.outbox.clone());
            if s.sessions.len() >= MAX_SESSIONS
                || sessions_of(s, from) >= MAX_SESSIONS_PER_MEMBER
                || sessions_of(s, me) >= MAX_SESSIONS_PER_MEMBER
            {
                return vec![(asker_box, ServerMsg::Refused(Refusal::Full))];
            }
            let mut id = s.next_session;
            // Bounded: fewer than MAX_SESSIONS ids are taken, so a free one is found quickly.
            while s.sessions.contains_key(&id) {
                id = id.wrapping_add(1);
            }
            s.next_session = id.wrapping_add(1);
            s.sessions.insert(
                id,
                Session {
                    a: (from, asker_conn),
                    b: (me, my_conn),
                },
            );
            vec![
                (asker_box, ServerMsg::RelayReady { session: id, peer: me }),
                (my_box, ServerMsg::RelayReady { session: id, peer: from }),
            ]
        }
    }
}

fn sessions_of(s: &State, key: PublicId) -> usize {
    s.sessions.values().filter(|session| session.involves(key)).count()
}

fn route(state: &Shared, session: u32, from: PublicId) -> Option<quinn::Connection> {
    let s = lock(state)?;
    s.sessions.get(&session)?.other(from).cloned()
}

/// Whether the server's total relay budget has room for `bytes` now.
fn within_total(state: &Shared, bytes: usize) -> bool {
    lock(state).is_some_and(|mut s| s.total.take(bytes, Instant::now()))
}

/// One member's bandwidth buckets, by relay session (`limit.rs`). Only sessions the member
/// is in get one, so there are at most `MAX_SESSIONS_PER_MEMBER`.
type Buckets = Arc<Mutex<HashMap<u32, Bucket>>>;

/// Whether `session` may carry `bytes` from this member now.
fn within_limit(buckets: &Buckets, session: u32, bytes: usize) -> bool {
    let now = std::time::Instant::now();
    let Ok(mut map) = buckets.lock() else {
        return false;
    };
    map.retain(|_, b| !b.idle(now));
    map.entry(session).or_insert_with(|| Bucket::new(now)).take(bytes, now)
}

async fn forward_datagrams(conn: quinn::Connection, me: PublicId, state: Shared, buckets: Buckets) {
    while let Ok(datagram) = conn.read_datagram().await {
        let Some((session, _)) = split_session(&datagram) else {
            continue;
        };
        if let Some(target) = route(&state, session, me)
            && within_limit(&buckets, session, datagram.len())
            && within_total(&state, datagram.len())
        {
            let _ = target.send_datagram(datagram);
        }
    }
}

async fn forward_streams(conn: quinn::Connection, me: PublicId, state: Shared, buckets: Buckets) {
    while let Ok(mut stream) = conn.accept_uni().await {
        let (state, buckets) = (state.clone(), buckets.clone());
        tokio::spawn(async move {
            let mut head = [0u8; STREAM_HEADER];
            if stream.read_exact(&mut head).await.is_err() {
                return;
            }
            let (session, len) = parse_stream_header(head);
            let Some(target) = route(&state, session, me) else {
                return;
            };
            if !within_limit(&buckets, session, len) || !within_total(&state, len) {
                return;
            }
            let mut packet = vec![0u8; len];
            if stream.read_exact(&mut packet).await.is_err() {
                return;
            }
            let Ok(mut out) = target.open_uni().await else {
                return;
            };
            if out.write_all(&head).await.is_ok() && out.write_all(&packet).await.is_ok() {
                let _ = out.finish();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol;

    #[test]
    fn session_routes_only_between_its_two_members() {
        let (a, b, stranger) = (PublicId([1; 32]), PublicId([2; 32]), PublicId([3; 32]));
        let s = Session { a: (a, 'A'), b: (b, 'B') };
        assert_eq!(s.other(a), Some(&'B'));
        assert_eq!(s.other(b), Some(&'A'));
        assert_eq!(s.other(stranger), None);
        assert!(s.involves(a) && !s.involves(stranger));
    }

    #[test]
    fn session_ids_reuse_protocol_framing() {
        let packet = protocol::session_packet(5, b"x");
        assert_eq!(split_session(&packet).map(|(id, _)| id), Some(5));
    }
}
