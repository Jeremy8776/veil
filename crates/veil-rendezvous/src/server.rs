//! The rendezvous and relay service. Runs on any Veil node.
//!
//! State lives behind one mutex that is never held across an `.await`. Each member link has
//! a reader loop (requests), a writer task (replies and notices) and two relay forwarders
//! (datagrams and the stream fallback).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::mpsc;
use veil_net::{Link, NetError, Node};
use veil_security::PublicId;

use crate::protocol::{
    ClientMsg, MAX_ADDRS, Refusal, STREAM_HEADER, ServerMsg, parse_stream_header,
    split_session,
};

pub const MAX_ROOMS: usize = 10_000;
pub const MAX_MEMBERS_PER_ROOM: usize = 64;
pub const MAX_SESSIONS: usize = 4_096;
const OUTBOX: usize = 64;

struct Member {
    addrs: Vec<SocketAddr>,
    outbox: mpsc::Sender<ServerMsg>,
    conn: quinn::Connection,
}

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

#[derive(Default)]
struct State {
    rooms: HashMap<[u8; 32], HashMap<PublicId, Member>>,
    sessions: HashMap<u32, Session<quinn::Connection>>,
    next_session: u32,
    relay_disabled: bool,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> Option<MutexGuard<'_, State>> {
    // A poisoned lock means a handler panicked; stop serving rather than trust the state.
    state.lock().ok()
}

pub struct RendezvousServer {
    node: Node,
    state: Shared,
}

impl RendezvousServer {
    pub fn new(node: Node) -> Self {
        Self {
            node,
            state: Shared::default(),
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
                tokio::spawn(serve(link, self.state.clone()));
            }
        }
    }
}

async fn serve(link: Link, state: Shared) {
    let Ok(control) = link.accept_control().await else {
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
    let datagrams = tokio::spawn(forward_datagrams(conn.clone(), me, state.clone()));
    let streams = tokio::spawn(forward_streams(conn, me, state.clone()));

    let mut room: Option<[u8; 32]> = None;
    while let Ok(message) = input.recv::<ClientMsg>().await {
        let replies = handle(message, &link, &mut room, &outbox, &state);
        for (to, reply) in replies {
            // A member that stops reading loses notices; it does not stall the server.
            let _ = to.try_send(reply);
        }
    }

    if let Some(mut s) = lock(&state) {
        if let Some(tag) = room {
            if let Some(members) = s.rooms.get_mut(&tag) {
                members.remove(&me);
                if members.is_empty() {
                    s.rooms.remove(&tag);
                }
            }
        }
        s.sessions.retain(|_, session| !session.involves(me));
    }
    writer.abort();
    datagrams.abort();
    streams.abort();
}

type Outgoing = Vec<(mpsc::Sender<ServerMsg>, ServerMsg)>;

fn handle(
    message: ClientMsg,
    link: &Link,
    room: &mut Option<[u8; 32]>,
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
            local_addrs,
        } => {
            if room.is_some() {
                return refuse(Refusal::AlreadyRegistered);
            }
            if local_addrs.len() > MAX_ADDRS {
                return refuse(Refusal::TooManyAddrs);
            }
            if !s.rooms.contains_key(&room_tag) && s.rooms.len() >= MAX_ROOMS {
                return refuse(Refusal::Full);
            }
            let members = s.rooms.entry(room_tag).or_default();
            if members.contains_key(&me) {
                return refuse(Refusal::AlreadyRegistered);
            }
            if members.len() >= MAX_MEMBERS_PER_ROOM {
                return refuse(Refusal::Full);
            }
            let observed = link.remote_addr();
            let mut addrs = vec![observed];
            addrs.extend(local_addrs.into_iter().filter(|a| *a != observed));
            members.insert(
                me,
                Member {
                    addrs,
                    outbox: outbox.clone(),
                    conn: link.quic().clone(),
                },
            );
            *room = Some(room_tag);
            vec![(outbox.clone(), ServerMsg::Registered { observed })]
        }

        ClientMsg::Introduce { peer } => {
            let Some(tag) = room else {
                return refuse(Refusal::NotRegistered);
            };
            let Some(members) = s.rooms.get(tag) else {
                return refuse(Refusal::NotRegistered);
            };
            let (Some(mine), Some(theirs)) = (members.get(&me), members.get(&peer)) else {
                return refuse(Refusal::PeerNotInRoom);
            };
            if peer == me {
                return refuse(Refusal::PeerNotInRoom);
            }
            // Fire together: the nearer member waits half the round-trip difference.
            let (my_rtt, their_rtt) = (mine.conn.rtt(), theirs.conn.rtt());
            let longest = my_rtt.max(their_rtt);
            let wait = |rtt: Duration| {
                u32::try_from((longest.saturating_sub(rtt) / 2).as_millis()).unwrap_or(u32::MAX)
            };
            vec![
                (
                    mine.outbox.clone(),
                    ServerMsg::Punch {
                        peer,
                        addrs: theirs.addrs.clone(),
                        delay_ms: wait(my_rtt),
                    },
                ),
                (
                    theirs.outbox.clone(),
                    ServerMsg::Punch {
                        peer: me,
                        addrs: mine.addrs.clone(),
                        delay_ms: wait(their_rtt),
                    },
                ),
            ]
        }

        ClientMsg::OpenRelay { peer } => {
            if s.relay_disabled {
                return refuse(Refusal::RelayDisabled);
            }
            let Some(tag) = room else {
                return refuse(Refusal::NotRegistered);
            };
            let pair = s.rooms.get(tag).and_then(|members| {
                let mine = members.get(&me)?;
                let theirs = members.get(&peer)?;
                (peer != me).then(|| {
                    (
                        (mine.conn.clone(), mine.outbox.clone()),
                        (theirs.conn.clone(), theirs.outbox.clone()),
                    )
                })
            });
            let Some(((my_conn, my_box), (their_conn, their_box))) = pair else {
                return refuse(Refusal::PeerNotInRoom);
            };
            if s.sessions.len() >= MAX_SESSIONS {
                return refuse(Refusal::Full);
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
                    a: (me, my_conn),
                    b: (peer, their_conn),
                },
            );
            vec![
                (my_box, ServerMsg::RelayReady { session: id, peer }),
                (their_box, ServerMsg::RelayReady { session: id, peer: me }),
            ]
        }
    }
}

fn route(state: &Shared, session: u32, from: PublicId) -> Option<quinn::Connection> {
    let s = lock(state)?;
    s.sessions.get(&session)?.other(from).cloned()
}

// shortcut: no per-session bandwidth cap or fair queuing. Add both before running a relay
// for strangers.
async fn forward_datagrams(conn: quinn::Connection, me: PublicId, state: Shared) {
    while let Ok(datagram) = conn.read_datagram().await {
        let Some((session, _)) = split_session(&datagram) else {
            continue;
        };
        if let Some(target) = route(&state, session, me) {
            let _ = target.send_datagram(datagram);
        }
    }
}

async fn forward_streams(conn: quinn::Connection, me: PublicId, state: Shared) {
    while let Ok(mut stream) = conn.accept_uni().await {
        let state = state.clone();
        tokio::spawn(async move {
            let mut head = [0u8; STREAM_HEADER];
            if stream.read_exact(&mut head).await.is_err() {
                return;
            }
            let (session, len) = parse_stream_header(head);
            let Some(target) = route(&state, session, me) else {
                return;
            };
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
