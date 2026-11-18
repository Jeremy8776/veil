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
//! - The room is registered under `room_tag(room_id)`, never the id itself, and named by its
//!   host's key. The host opens it and keeps the rendezvous node's allow list current from its
//!   session's [`Allowed`]; a joiner registers with its invite's lobby pass ([`Seat`]).
//! - Every relayed link is the two participants' own pinned QUIC link. The relay carries
//!   ciphertext and never holds keys.
//! - A punch dial-back is never kept. The peer that asked dials the link that is used; the
//!   dial-back only opens this side's NAT mapping. This keeps one link per pair.
//! - Another member's request (introduction or relay) is accepted only as [`Consent`]
//!   allows: the host takes anyone (joiners dial its address, and the lobby decides who gets
//!   in); a member takes only peers its current routes expect; a relay-only node never
//!   accepts an introduction, so its address never leaves the rendezvous. A request that
//!   arrives just before the routes that expect it waits up to [`WAIT`] and is accepted
//!   when they come.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use veil_net::Node;
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::{Ask, Punch, RendezvousClient, punch, room_tag};
use veil_security::{Identity, PublicId};

use crate::CallError;
use crate::host::Allowed;
use crate::inbound::Router;
use crate::relay_field::RelayAddrs;

/// How this participant registers in the call's rendezvous room.
pub(crate) enum Seat {
    /// The host: opens the room and keeps its allow list current.
    Host(watch::Receiver<Allowed>),
    /// A joiner: registers in `host`'s room with its invite's lobby pass. Once admitted it is
    /// on the host's allow list, so the pass is no longer needed.
    Guest { host: PublicId, pass: [u8; 32] },
}

const COMMANDS: usize = 16;
/// Relay sessions other members opened to this one. Sixteen is the call's size limit.
const MAX_RELAYED: usize = 32;
/// Requests kept while no route expects their sender yet. The server forgets them at 10 s.
const WAIT: Duration = Duration::from_secs(8);
const MAX_WAITING: usize = 64;

enum Command {
    Introduce(PublicId, oneshot::Sender<Result<Punch, CallError>>),
    Relay(PublicId, oneshot::Sender<Result<Arc<Node>, CallError>>),
    /// The expected peers changed: accept waiting requests that are now allowed.
    Recheck,
}

/// Whose requests this participant accepts. Shared with its `CallNode` and media session.
#[derive(Default)]
pub(crate) struct Consent {
    /// The host: anyone holding the invite may be introduced or relayed to it.
    pub open: bool,
    pub relay_only: AtomicBool,
    /// Peers the current routes link to.
    pub expected: Mutex<HashSet<PublicId>>,
}

impl Consent {
    pub fn allows(&self, from: &PublicId, kind: Ask) -> bool {
        if kind == Ask::Introduce && self.relay_only.load(Ordering::Relaxed) {
            return false;
        }
        self.open || self.expected.lock().is_ok_and(|e| e.contains(from))
    }
}

/// Talks to the presence task. Cheap to clone.
#[derive(Clone)]
pub(crate) struct Presence {
    commands: mpsc::Sender<Command>,
    consent: Arc<Consent>,
}

impl Presence {
    /// Connects to the rendezvous node and registers this node in the room.
    pub async fn start(
        identity: &Identity,
        node: Arc<Node>,
        server: &RelayAddrs,
        router: Router,
        consent: Arc<Consent>,
        seat: Seat,
    ) -> Result<(Self, tokio::task::JoinHandle<()>), CallError> {
        let mut client = RendezvousClient::connect(&node, &server.addrs, server.key)
            .await
            .map_err(CallError::Rendezvous)?;
        let local = node.local_addr()?;
        let locals = if local.ip().is_unspecified() { Vec::new() } else { vec![local] };
        let (host, pass, mut allowed) = match seat {
            Seat::Host(allowed) => (identity.public(), None, Some(allowed)),
            Seat::Guest { host, pass } => (host, Some(pass), None),
        };
        client
            .register(room_tag(&router.room_id), host, pass, locals)
            .await
            .map_err(CallError::Rendezvous)?;
        if let Some(allowed) = allowed.as_mut() {
            let now = allowed.borrow_and_update().clone();
            client
                .allow(now.members, now.passes)
                .await
                .map_err(CallError::Rendezvous)?;
        }
        let policy = consent.clone();
        client.set_consent(Arc::new(move |from, kind| policy.allows(from, kind)));
        let (commands, rx) = mpsc::channel(COMMANDS);
        let actor = Actor {
            identity: Identity::from_secret(&identity.secret_bytes()),
            node,
            client,
            router,
            relayed: JoinSet::new(),
            consent: consent.clone(),
            waiting: VecDeque::new(),
            allowed,
        };
        Ok((Self { commands, consent }, tokio::spawn(actor.run(rx))))
    }

    /// The peers this participant's routes now link to. Their requests are accepted, and
    /// so are any of theirs that arrived a moment early.
    pub fn expect(&self, peers: impl IntoIterator<Item = PublicId>) {
        if let Ok(mut expected) = self.consent.expected.lock() {
            *expected = peers.into_iter().collect();
        }
        // A full queue already holds commands; the next recheck sees the new set.
        let _ = self.commands.try_send(Command::Recheck);
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
    consent: Arc<Consent>,
    /// Requests not yet allowed, oldest first, with when they came.
    waiting: VecDeque<(PublicId, Ask, Instant)>,
    /// The host's allow list, while this participant hosts.
    allowed: Option<watch::Receiver<Allowed>>,
}

/// The host's next allow list. Never resolves for a joiner.
async fn next_allowed(allowed: &mut Option<watch::Receiver<Allowed>>) -> Option<Allowed> {
    let Some(rx) = allowed.as_mut() else {
        return std::future::pending().await;
    };
    rx.changed().await.ok()?;
    Some(rx.borrow_and_update().clone())
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
                    Some(notice) => self.notice(notice).await,
                    // The rendezvous link closed. Requests now fail with `Closed`.
                    None => break,
                },
                allowed = next_allowed(&mut self.allowed) => match allowed {
                    // A refused or lost update leaves the last list standing.
                    Some(now) => {
                        let _ = self.client.allow(now.members, now.passes).await;
                    }
                    // The host session ended.
                    None => self.allowed = None,
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
            Command::Recheck => {
                let now = Instant::now();
                self.waiting.retain(|(.., at)| now.duration_since(*at) < WAIT);
                for (from, kind, at) in std::mem::take(&mut self.waiting) {
                    if self.consent.allows(&from, kind) {
                        let _ = self.client.accept(from, kind).await;
                    } else {
                        self.waiting.push_back((from, kind, at));
                    }
                }
            }
        }
    }

    /// Accepts another member's request if consent allows; otherwise keeps it a moment, in
    /// case the routes that expect that member are about to arrive.
    async fn request(&mut self, from: PublicId, kind: Ask) {
        if self.consent.allows(&from, kind) {
            let _ = self.client.accept(from, kind).await;
            return;
        }
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back((from, kind, Instant::now()));
    }

    async fn open_relay(&mut self, peer: PublicId) -> Result<Arc<Node>, CallError> {
        let session = self.client.open_relay(peer).await.map_err(CallError::Rendezvous)?;
        let socket = self.client.relay_socket(session).map_err(CallError::Rendezvous)?;
        Ok(Arc::new(Node::bind_virtual(&self.identity, socket)?))
    }

    async fn notice(&mut self, notice: ServerMsg) {
        match notice {
            ServerMsg::Request { from, kind } => self.request(from, kind).await,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_follows_role_routes_and_relay_only() {
        let (known, stranger) = (PublicId([1; 32]), PublicId([2; 32]));
        let member = Consent::default();
        assert!(!member.allows(&known, Ask::Introduce), "nobody before routes");
        member.expected.lock().map(|mut e| e.insert(known)).ok();
        assert!(member.allows(&known, Ask::Introduce));
        assert!(member.allows(&known, Ask::Relay));
        assert!(!member.allows(&stranger, Ask::Introduce), "an invite alone is not enough");
        assert!(!member.allows(&stranger, Ask::Relay));
        member.relay_only.store(true, Ordering::Relaxed);
        assert!(!member.allows(&known, Ask::Introduce), "relay-only hides its address");
        assert!(member.allows(&known, Ask::Relay), "but still takes relayed links");

        let host = Consent {
            open: true,
            ..Consent::default()
        };
        assert!(host.allows(&stranger, Ask::Introduce), "joiners dial the host");
        host.relay_only.store(true, Ordering::Relaxed);
        assert!(!host.allows(&stranger, Ask::Introduce));
        assert!(host.allows(&stranger, Ask::Relay));
    }
}
