//! Client side of rendezvous: register, get introduced, open relay sessions.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use veil_net::{FramedSend, Link, NetError, Node};
use veil_security::PublicId;

use crate::protocol::{ClientMsg, STREAM_HEADER, ServerMsg, parse_stream_header, split_session};
use crate::{RelaySocket, RendezvousError};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING: usize = 64;
const INBOX: usize = 1_024;

type Inboxes = Arc<Mutex<HashMap<u32, mpsc::Sender<Bytes>>>>;

/// Instructions to dial a peer for hole punching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Punch {
    pub peer: PublicId,
    pub addrs: Vec<SocketAddr>,
    pub delay: Duration,
}

/// Waits the agreed delay, then dials. Both members do this at once.
// When both dials succeed there are two links. `veil-call` keeps only the one dialled by the
// member that asked for the introduction and closes the other side's dial-back.
pub async fn punch(node: &Node, punch: &Punch) -> Result<Link, NetError> {
    tokio::time::sleep(punch.delay).await;
    node.connect(&punch.addrs, punch.peer).await
}

pub struct RendezvousClient {
    link: Link,
    out: FramedSend,
    events: mpsc::Receiver<ServerMsg>,
    pending: VecDeque<ServerMsg>,
    inboxes: Inboxes,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for RendezvousClient {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl RendezvousClient {
    /// Connects to a rendezvous node, pinned to its key.
    pub async fn connect(
        node: &Node,
        addrs: &[SocketAddr],
        server: PublicId,
    ) -> Result<Self, RendezvousError> {
        let link = node.connect(addrs, server).await?;
        let (out, mut input) = link.open_control().await?.split();
        let (tx, events) = mpsc::channel(MAX_PENDING);
        let inboxes = Inboxes::default();
        let conn = link.quic().clone();

        let reader = tokio::spawn(async move {
            while let Ok(message) = input.recv::<ServerMsg>().await {
                if tx.send(message).await.is_err() {
                    break;
                }
            }
        });
        let datagrams = tokio::spawn(demux_datagrams(conn.clone(), inboxes.clone()));
        let streams = tokio::spawn(demux_streams(conn, inboxes.clone()));
        Ok(Self {
            link,
            out,
            events,
            pending: VecDeque::new(),
            inboxes,
            tasks: vec![reader, datagrams, streams],
        })
    }

    pub fn server(&self) -> PublicId {
        self.link.peer()
    }

    /// Registers in a room. Returns the public address the server sees.
    pub async fn register(
        &mut self,
        room_tag: [u8; 32],
        local_addrs: Vec<SocketAddr>,
    ) -> Result<SocketAddr, RendezvousError> {
        self.out
            .send(&ClientMsg::Register {
                room_tag,
                local_addrs,
            })
            .await?;
        match self
            .answer(|m| matches!(m, ServerMsg::Registered { .. }))
            .await?
        {
            ServerMsg::Registered { observed } => Ok(observed),
            _ => Err(RendezvousError::Closed),
        }
    }

    /// Asks to be introduced to `peer`. The peer receives a matching [`ServerMsg::Punch`].
    pub async fn introduce(&mut self, peer: PublicId) -> Result<Punch, RendezvousError> {
        self.out.send(&ClientMsg::Introduce { peer }).await?;
        match self
            .answer(|m| matches!(m, ServerMsg::Punch { peer: p, .. } if *p == peer))
            .await?
        {
            ServerMsg::Punch {
                peer,
                addrs,
                delay_ms,
            } => Ok(Punch {
                peer,
                addrs,
                delay: Duration::from_millis(u64::from(delay_ms)),
            }),
            _ => Err(RendezvousError::Closed),
        }
    }

    /// Opens a relay session with `peer` and returns its session id.
    pub async fn open_relay(&mut self, peer: PublicId) -> Result<u32, RendezvousError> {
        self.out.send(&ClientMsg::OpenRelay { peer }).await?;
        match self
            .answer(|m| matches!(m, ServerMsg::RelayReady { peer: p, .. } if *p == peer))
            .await?
        {
            ServerMsg::RelayReady { session, .. } => Ok(session),
            _ => Err(RendezvousError::Closed),
        }
    }

    /// The next notice from the server, such as another member's punch or relay request.
    pub async fn next_event(&mut self) -> Option<ServerMsg> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        self.events.recv().await
    }

    /// A virtual socket for a relay session. Bind a node to it with [`Node::bind_virtual`].
    pub fn relay_socket(&self, session: u32) -> Result<Arc<RelaySocket>, RendezvousError> {
        let (tx, rx) = mpsc::channel(INBOX);
        self.inboxes
            .lock()
            .map_err(|_| RendezvousError::Closed)?
            .insert(session, tx);
        Ok(Arc::new(RelaySocket::new(self.link.quic().clone(), session, rx)))
    }

    /// Waits for the reply `want` describes, or a refusal. Other notices are kept for
    /// [`Self::next_event`].
    async fn answer(
        &mut self,
        want: impl Fn(&ServerMsg) -> bool,
    ) -> Result<ServerMsg, RendezvousError> {
        let deadline = tokio::time::Instant::now() + ANSWER_TIMEOUT;
        loop {
            let message = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .map_err(|_| RendezvousError::Timeout)?
                .ok_or(RendezvousError::Closed)?;
            if let ServerMsg::Refused(reason) = message {
                return Err(RendezvousError::Refused(reason));
            }
            if want(&message) {
                return Ok(message);
            }
            if self.pending.len() >= MAX_PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back(message);
        }
    }
}

fn deliver(inboxes: &Inboxes, session: u32, packet: Bytes) {
    let Ok(map) = inboxes.lock() else {
        return;
    };
    if let Some(inbox) = map.get(&session) {
        // A full inbox drops the packet, like a congested network. QUIC recovers.
        let _ = inbox.try_send(packet);
    }
}

async fn demux_datagrams(conn: quinn::Connection, inboxes: Inboxes) {
    while let Ok(datagram) = conn.read_datagram().await {
        let Some((session, packet)) = split_session(&datagram) else {
            continue;
        };
        let start = datagram.len() - packet.len();
        deliver(&inboxes, session, datagram.slice(start..));
    }
}

async fn demux_streams(conn: quinn::Connection, inboxes: Inboxes) {
    while let Ok(mut stream) = conn.accept_uni().await {
        let inboxes = inboxes.clone();
        tokio::spawn(async move {
            let mut head = [0u8; STREAM_HEADER];
            if stream.read_exact(&mut head).await.is_err() {
                return;
            }
            let (session, len) = parse_stream_header(head);
            let mut packet = vec![0u8; len];
            if stream.read_exact(&mut packet).await.is_ok() {
                deliver(&inboxes, session, Bytes::from(packet));
            }
        });
    }
}
