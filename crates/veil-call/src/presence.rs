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
//! - A relay session someone else opened holds a place only until its link arrives, or for
//!   [`RELAY_WAIT`] if none does ([`RelayPlaces`]).
//! - A dial-back happens only for an introduction this participant just agreed to, within the
//!   limits in [`DialBacks`]. Any other `Punch` notice is ignored, whoever sent it.
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
use veil_net::{Node, NodeAddr};
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::{Ask, Punch, RendezvousClient, punch, room_tag};
use veil_security::{Identity, PublicId};

use crate::CallError;
use crate::dial_back::DialBacks;
use crate::host::Allowed;
use crate::inbound::Router;
use crate::relay_field::RelayAddrs;
use crate::relay_places::RelayPlaces;

/// How this participant registers in the call's rendezvous room.
pub(crate) enum Seat {
    /// The host: opens the room and keeps its allow list current.
    Host(watch::Receiver<Allowed>),
    /// A joiner: registers in `host`'s room with its invite's lobby pass. Once admitted it is
    /// on the host's allow list, so the pass is no longer needed.
    Guest { host: PublicId, pass: [u8; 32] },
}

const COMMANDS: usize = 16;
/// How long a relay session someone else opened waits for its link: a dial's whole allowance
/// (`veil_net::endpoint::CONNECT_TIMEOUT`), and a little more.
const RELAY_WAIT: Duration = Duration::from_secs(10);
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
    /// Introductions agreed to, and the dial-backs they led to.
    pub dial_backs: Mutex<DialBacks>,
}

impl Consent {
    pub fn allows(&self, from: &PublicId, kind: Ask) -> bool {
        if kind == Ask::Introduce && self.relay_only.load(Ordering::Relaxed) {
            return false;
        }
        self.open || self.expected.lock().is_ok_and(|e| e.contains(from))
    }

    /// Whether to accept this request. Accepting an introduction allows one dial-back.
    pub fn agree(&self, from: &PublicId, kind: Ask) -> bool {
        let allowed = self.allows(from, kind);
        if allowed
            && kind == Ask::Introduce
            && let Ok(mut backs) = self.dial_backs.lock()
        {
            backs.agree(*from, Instant::now());
        }
        allowed
    }

    /// Whether a `Punch` notice for `peer`, naming `addrs` addresses, should be dialled.
    fn may_dial_back(&self, peer: &PublicId, addrs: usize) -> bool {
        self.allows(peer, Ask::Introduce)
            && self
                .dial_backs
                .lock()
                .is_ok_and(|mut backs| backs.allow(*peer, addrs, Instant::now()))
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
        client.set_consent(Arc::new(move |from, kind| policy.agree(from, kind)));
        let (commands, rx) = mpsc::channel(COMMANDS);
        let actor = Actor {
            identity: Identity::from_secret(&identity.secret_bytes()),
            node,
            client,
            router,
            relayed: RelayPlaces::default(),
            dial_backs: JoinSet::new(),
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
    /// Relay sessions others opened, waiting for their links. Dropped with the actor.
    relayed: RelayPlaces,
    /// Dial-backs under way. Dropped with the actor.
    dial_backs: JoinSet<()>,
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
                    if self.consent.agree(&from, kind) {
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
        if self.consent.agree(&from, kind) {
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
                while self.dial_backs.try_join_next().is_some() {}
                if !self.consent.may_dial_back(&peer, addrs.len()) {
                    return;
                }
                let plan = Punch {
                    peer,
                    addrs,
                    delay: std::time::Duration::from_millis(u64::from(delay_ms)),
                };
                let node = self.node.clone();
                self.dial_backs.spawn(async move {
                    if let Ok(link) = punch(&node, &plan).await {
                        link.close("punch only");
                    }
                });
            }
            ServerMsg::RelayReady { session, peer } => {
                let Ok(socket) = self.client.relay_socket(session) else {
                    return;
                };
                let Ok(inner) = Node::bind_virtual(&self.identity, socket) else {
                    return;
                };
                let wait = self.router.clone().accept_relayed(Arc::new(inner), RELAY_WAIT);
                self.relayed.start(peer, wait);
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

    #[test]
    fn only_an_agreed_introduction_allows_a_dial_back() {
        let peer = PublicId([1; 32]);
        let host = Consent {
            open: true,
            ..Consent::default()
        };
        assert!(!host.may_dial_back(&peer, 1), "nothing was agreed");
        assert!(host.agree(&peer, Ask::Relay));
        assert!(!host.may_dial_back(&peer, 1), "a relay is not an introduction");
        assert!(host.agree(&peer, Ask::Introduce));
        assert!(host.may_dial_back(&peer, 1));

        // Agreed, then this node went relay-only before the notice came.
        let other = PublicId([2; 32]);
        assert!(host.agree(&other, Ask::Introduce));
        host.relay_only.store(true, Ordering::Relaxed);
        assert!(!host.may_dial_back(&other, 1));
        assert!(!host.agree(&other, Ask::Introduce), "and it agrees to no more");
    }
}
