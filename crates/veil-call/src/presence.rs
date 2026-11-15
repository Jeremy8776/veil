//! A participant's presence on a rendezvous node: introductions, punching and relay sessions.
//!
//! One task owns the [`RendezvousClient`], so requests and the server's notices are handled in
//! one order. It does two things:
//!
//! - **Requests** from this participant: introduce me to a peer (for a punched dial), or open
//!   a relay session with a peer and give me a virtual node to dial over it.
//! - **Notices** from the server: a peer asked to punch to me, so dial back once to open my
//!   router's mapping; or a peer opened a relay session, so bind a virtual node for it and
//!   route whatever links arrive on it, exactly like links on the real socket.
//!
//! Invariants:
//! - The room is registered under `room_tag(room_id)`, never the id itself.
//! - Every relayed link is the two participants' own pinned QUIC link. The relay carries
//!   ciphertext and never holds keys.
//! - A punch dial-back is never kept. The peer that asked dials the link that is used; the
//!   dial-back only opens this side's NAT mapping. This keeps one link per pair.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use veil_net::{Node, NodeAddr};
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::{Punch, RendezvousClient, punch, room_tag};
use veil_security::{Identity, PublicId};

use crate::CallError;
use crate::inbound::Router;

const COMMANDS: usize = 16;
/// Relay sessions other members opened to this one. Sixteen is the call's size limit.
const MAX_RELAYED: usize = 32;

enum Command {
    Introduce(PublicId, oneshot::Sender<Result<Punch, CallError>>),
    Relay(PublicId, oneshot::Sender<Result<Arc<Node>, CallError>>),
}

/// Talks to the presence task. Cheap to clone.
#[derive(Clone)]
pub(crate) struct Presence {
    commands: mpsc::Sender<Command>,
}

impl Presence {
    /// Connects to the rendezvous node and registers this node in the room.
    pub async fn start(
        identity: &Identity,
        node: Arc<Node>,
        server: NodeAddr,
        router: Router,
    ) -> Result<(Self, tokio::task::JoinHandle<()>), CallError> {
        let mut client = RendezvousClient::connect(&node, &[server.addr], server.key)
            .await
            .map_err(CallError::Rendezvous)?;
        let local = node.local_addr()?;
        let locals = if local.ip().is_unspecified() { Vec::new() } else { vec![local] };
        client
            .register(room_tag(&router.room_id), locals)
            .await
            .map_err(CallError::Rendezvous)?;
        let (commands, rx) = mpsc::channel(COMMANDS);
        let actor = Actor {
            identity: Identity::from_secret(&identity.secret_bytes()),
            node,
            client,
            router,
            relayed: JoinSet::new(),
        };
        Ok((Self { commands }, tokio::spawn(actor.run(rx))))
    }

    /// Asks the server to introduce this node to `peer`. Both then dial at the given moment.
    pub async fn introduce(&self, peer: PublicId) -> Result<Punch, CallError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Introduce(peer, reply))
            .await
            .map_err(|_| CallError::Closed)?;
        answer.await.map_err(|_| CallError::Closed)?
    }

    /// Opens a relay session with `peer`. Dial `peer` at `veil_rendezvous::socket::RELAY_PEER`
    /// on the returned node, and keep the node for as long as the link.
    pub async fn relay(&self, peer: PublicId) -> Result<Arc<Node>, CallError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Relay(peer, reply))
            .await
            .map_err(|_| CallError::Closed)?;
        answer.await.map_err(|_| CallError::Closed)?
    }
}

struct Actor {
    identity: Identity,
    node: Arc<Node>,
    client: RendezvousClient,
    router: Router,
    /// Accept loops of relay sessions others opened. Dropped with the actor.
    relayed: JoinSet<()>,
}

impl Actor {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.command(command).await,
                    None => break,
                },
                notice = self.client.next_event() => match notice {
                    Some(notice) => self.notice(notice),
                    // The rendezvous link closed. Requests now fail with `Closed`.
                    None => break,
                },
            }
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Introduce(peer, reply) => {
                let result = self.client.introduce(peer).await.map_err(CallError::Rendezvous);
                let _ = reply.send(result);
            }
            Command::Relay(peer, reply) => {
                let _ = reply.send(self.open_relay(peer).await);
            }
        }
    }

    async fn open_relay(&mut self, peer: PublicId) -> Result<Arc<Node>, CallError> {
        let session = self.client.open_relay(peer).await.map_err(CallError::Rendezvous)?;
        let socket = self.client.relay_socket(session).map_err(CallError::Rendezvous)?;
        Ok(Arc::new(Node::bind_virtual(&self.identity, socket)?))
    }

    fn notice(&mut self, notice: ServerMsg) {
        match notice {
            ServerMsg::Punch {
                peer,
                addrs,
                delay_ms,
            } => {
                let plan = Punch {
                    peer,
                    addrs,
                    delay: std::time::Duration::from_millis(u64::from(delay_ms)),
                };
                let node = self.node.clone();
                tokio::spawn(async move {
                    if let Ok(link) = punch(&node, &plan).await {
                        link.close("punch only");
                    }
                });
            }
            ServerMsg::RelayReady { session, .. } => {
                // Finished loops are reaped; past the limit a new session is ignored, so a
                // misbehaving peer cannot grow this set without bound.
                while self.relayed.try_join_next().is_some() {}
                if self.relayed.len() >= MAX_RELAYED {
                    return;
                }
                let Ok(socket) = self.client.relay_socket(session) else {
                    return;
                };
                let Ok(inner) = Node::bind_virtual(&self.identity, socket) else {
                    return;
                };
                self.relayed
                    .spawn(self.router.clone().accept_loop(Arc::new(inner), true));
            }
            _ => {}
        }
    }
}
