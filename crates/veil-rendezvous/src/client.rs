//! Client side of rendezvous: register, get introduced, open relay sessions.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use veil_net::{FramedSend, Link, NetError, Node, NodeAddr};
use veil_security::PublicId;
use veil_security::short_invite::SealedInvite;

use crate::protocol::{
    Ask, ClientMsg, STREAM_HEADER, ServerMsg, parse_stream_header, split_session,
};
use crate::{RelaySocket, RendezvousError};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// The server's dial limit, plus room for the round trip.
const REACH_TIMEOUT: Duration = Duration::from_secs(8);

/// The answer to a reachability check (`reach.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reach {
    /// The address the server dialled: this machine's public IP as it sees it, at the port
    /// that was asked for.
    pub addr: SocketAddr,
    /// Whether the dial got through.
    pub open: bool,
}

/// Asks `server` to dial this machine's public IP at `port`, over a link of its own that is
/// closed afterwards, so a server too old to know the message cannot close a link in use.
pub async fn check_reach(
    node: &Node,
    server: &NodeAddr,
    port: u16,
) -> Result<Reach, RendezvousError> {
    let mut client = RendezvousClient::connect(node, &[server.addr], server.key).await?;
    let reach = client.check_reach(port).await;
    client.link.close("done");
    reach
}
const MAX_PENDING: usize = 64;
const INBOX: usize = 1_024;
/// The longest a punch waits before dialling. The server only evens out the two members'
/// round trips, so a longer delay in a notice is not one to honour.
pub const MAX_PUNCH_DELAY: Duration = Duration::from_secs(2);

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
    tokio::time::sleep(punch.delay.min(MAX_PUNCH_DELAY)).await;
    node.connect(&punch.addrs, punch.peer).await
}

pub struct RendezvousClient {
    link: Link,
    out: FramedSend,
    events: mpsc::Receiver<ServerMsg>,
    pending: VecDeque<ServerMsg>,
    inboxes: Inboxes,
    tasks: Vec<JoinHandle<()>>,
    consent: Option<Policy>,
}

/// Whether to accept a request from this member for this.
pub type Policy = Arc<dyn Fn(&PublicId, Ask) -> bool + Send + Sync>;

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
        Self::attach(node.connect(addrs, server).await?).await
    }

    /// Starts the client on a link already open.
    pub(crate) async fn attach(link: Link) -> Result<Self, RendezvousError> {
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
            consent: None,
        })
    }

    pub fn server(&self) -> PublicId {
        self.link.peer()
    }

    /// Registers in `host`'s room. The host passes its own key and no pass; anyone else needs
    /// to be on the host's allow list or show an open lobby pass. Returns the public address
    /// the server sees.
    pub async fn register(
        &mut self,
        room_tag: [u8; 32],
        host: PublicId,
        pass: Option<[u8; 32]>,
        local_addrs: Vec<SocketAddr>,
    ) -> Result<SocketAddr, RendezvousError> {
        self.out
            .send(&ClientMsg::Register {
                room_tag,
                host,
                pass,
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

    /// Host only: replaces who may be in the room (see [`ClientMsg::Allow`]). The server drops
    /// anyone the new lists no longer cover. Returns once the server has applied them.
    pub async fn allow(
        &mut self,
        members: Vec<PublicId>,
        passes: Vec<[u8; 32]>,
    ) -> Result<(), RendezvousError> {
        self.out.send(&ClientMsg::Allow { members, passes }).await?;
        self.answer(|m| matches!(m, ServerMsg::Allowed)).await?;
        Ok(())
    }

    /// Asks to be introduced to `peer`. The peer is asked first ([`ServerMsg::Request`]);
    /// once it accepts, both receive a matching [`ServerMsg::Punch`]. Times out if it does not.
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

    /// Opens a relay session with `peer` and returns its session id. The peer must accept.
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

    /// Asks the server to dial this machine's public IP at `port` from another socket. Prefer
    /// [`check_reach`], which uses a link of its own (see [`ClientMsg::CheckReach`]).
    pub async fn check_reach(&mut self, port: u16) -> Result<Reach, RendezvousError> {
        self.out.send(&ClientMsg::CheckReach { port }).await?;
        match self
            .answer_within(REACH_TIMEOUT, |m| matches!(m, ServerMsg::Reach { .. }))
            .await?
        {
            ServerMsg::Reach { addr, open } => Ok(Reach { addr, open }),
            _ => Err(RendezvousError::Closed),
        }
    }

    /// Host only: leaves a sealed invite in the room. Prefer [`crate::store_invite`], which
    /// uses a link of its own (see [`ClientMsg::PutInvite`]).
    pub async fn put_invite(
        &mut self,
        room_tag: [u8; 32],
        sealed: SealedInvite,
    ) -> Result<(), RendezvousError> {
        let SealedInvite { id, bytes } = sealed;
        self.out
            .send(&ClientMsg::PutInvite {
                room_tag,
                id,
                sealed: bytes,
            })
            .await?;
        self.answer(|m| matches!(m, ServerMsg::InviteStored))
            .await?;
        Ok(())
    }

    /// Asks for the sealed invite stored under `id`.
    pub async fn get_invite(&mut self, id: [u8; 32]) -> Result<Option<Vec<u8>>, RendezvousError> {
        self.out.send(&ClientMsg::GetInvite { id }).await?;
        match self
            .answer(|m| matches!(m, ServerMsg::SealedInvite { .. }))
            .await?
        {
            ServerMsg::SealedInvite { sealed } => Ok(sealed),
            _ => Err(RendezvousError::Closed),
        }
    }

    /// Ends this registration and its relay sockets. A later client must register again.
    pub fn close(&self) {
        self.link.close("done");
    }

    /// Requests this policy allows are accepted even while waiting for another answer, so
    /// two members asking each other at once do not each wait out the other. Requests it
    /// does not allow, or all of them without a policy, come out of [`Self::next_event`].
    pub fn set_consent(&mut self, allows: Policy) {
        self.consent = Some(allows);
    }

    /// Agrees to another member's [`ServerMsg::Request`]. Only accept members you expect.
    pub async fn accept(&mut self, from: PublicId, kind: Ask) -> Result<(), RendezvousError> {
        self.out.send(&ClientMsg::Accept { from, kind }).await?;
        Ok(())
    }

    /// The next notice from the server: another member's request, punch or relay session.
    pub async fn next_event(&mut self) -> Option<ServerMsg> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        self.events.recv().await
    }

    /// A virtual socket for a relay session. Bind a node to it with [`Node::bind_virtual`].
    pub fn relay_socket(&self, session: u32) -> Result<Arc<RelaySocket>, RendezvousError> {
        let (tx, rx) = mpsc::channel(INBOX);
        let mut inboxes = self.inboxes.lock().map_err(|_| RendezvousError::Closed)?;
        add_inbox(&mut inboxes, session, tx);
        drop(inboxes);
        Ok(Arc::new(RelaySocket::new(
            self.link.quic().clone(),
            session,
            rx,
        )))
    }

    /// Waits for the reply `want` describes, or a refusal. Other notices are kept for
    /// [`Self::next_event`].
    async fn answer(
        &mut self,
        want: impl Fn(&ServerMsg) -> bool,
    ) -> Result<ServerMsg, RendezvousError> {
        self.answer_within(ANSWER_TIMEOUT, want).await
    }

    async fn answer_within(
        &mut self,
        wait: Duration,
        want: impl Fn(&ServerMsg) -> bool,
    ) -> Result<ServerMsg, RendezvousError> {
        let deadline = tokio::time::Instant::now() + wait;
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
            // Two members asking each other at once would each wait for the other's answer.
            if let ServerMsg::Request { from, kind } = message
                && self
                    .consent
                    .as_ref()
                    .is_some_and(|allows| allows(&from, kind))
            {
                self.out.send(&ClientMsg::Accept { from, kind }).await?;
                continue;
            }
            if self.pending.len() >= MAX_PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back(message);
        }
    }
}

/// Adds a session's inbox, and forgets those whose socket has gone.
fn add_inbox(
    inboxes: &mut HashMap<u32, mpsc::Sender<Bytes>>,
    session: u32,
    inbox: mpsc::Sender<Bytes>,
) {
    inboxes.retain(|_, open| !open.is_closed());
    inboxes.insert(session, inbox);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_inbox_clears_those_of_closed_sockets() {
        let mut inboxes = HashMap::new();
        let (gone, gone_rx) = mpsc::channel(1);
        let (open, _open_rx) = mpsc::channel(1);
        let (new, _new_rx) = mpsc::channel(1);
        add_inbox(&mut inboxes, 1, gone);
        add_inbox(&mut inboxes, 2, open);
        drop(gone_rx);
        add_inbox(&mut inboxes, 3, new);
        let mut left: Vec<u32> = inboxes.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(left, [2, 3]);
    }
}
