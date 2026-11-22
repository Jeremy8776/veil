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
use veil_net::Node;
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
/// Reconnection retries never hammer a missing helper; the delay doubles to this cap.
const RECONNECT_MAX: Duration = Duration::from_secs(16);

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
    connected: Arc<AtomicBool>,
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
        let (host, pass, mut allowed) = match seat {
            Seat::Host(allowed) => (identity.public(), None, Some(allowed)),
            Seat::Guest { host, pass } => (host, Some(pass), None),
        };
        let initial = async {
            let mut client = RendezvousClient::connect(&node, &server.addrs, server.key).await?;
            let locals = veil_net::family::own_candidates(node.local_addr()?);
            client
                .register(room_tag(&router.room_id), host, pass, locals)
                .await?;
            if let Some(allowed) = allowed.as_mut() {
                let now = allowed.borrow_and_update().clone();
                client.allow(now.members, now.passes).await?;
            }
            Ok::<_, veil_rendezvous::RendezvousError>(client)
        }
        .await;
        let client = match initial {
            Ok(mut client) => {
                let policy = consent.clone();
                client.set_consent(Arc::new(move |from, kind| policy.agree(from, kind)));
                Some(client)
            }
            // An explicit refusal remains visible to the caller; do not obscure host ACLs.
            Err(error @ veil_rendezvous::RendezvousError::Refused(_)) => {
                return Err(CallError::Rendezvous(error));
            }
            // A temporarily unavailable helper is optional. Keep trying in the background
            // while direct calls proceed instead of permanently losing this presence.
            Err(_) => None,
        };
        let (commands, rx) = mpsc::channel(COMMANDS);
        let connected = Arc::new(AtomicBool::new(client.is_some()));
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
            server: server.clone(),
            host,
            pass,
            connected: connected.clone(),
        };
        Ok((
            Self {
                commands,
                consent,
                connected,
            },
            tokio::spawn(actor.run(rx)),
        ))
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
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
    client: Option<RendezvousClient>,
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
    server: RelayAddrs,
    host: PublicId,
    pass: Option<[u8; 32]>,
    connected: Arc<AtomicBool>,
}

impl Drop for Actor {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::Release);
    }
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
            if self.client.is_none() && !self.recover(&mut commands).await {
                break;
            }
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.command(command).await,
                    None => break,
                },
                notice = next_notice(&mut self.client) => match notice {
                    Some(notice) => self.notice(notice).await,
                    // Existing direct links survive. Restore registration and the latest
                    // permissions before accepting any new helper traffic.
                    None => if !self.recover(&mut commands).await { break },
                },
                allowed = next_allowed(&mut self.allowed) => match allowed {
                    // A refused or lost update leaves the last list standing.
                    Some(now) => {
                        let restored = match self.client.as_mut() {
                            Some(client) => client.allow(now.members, now.passes).await.is_ok(),
                            None => false,
                        };
                        if !restored && !self.recover(&mut commands).await
                        {
                            break;
                        }
                    }
                    // The host session ended.
                    None => self.allowed = None,
                },
            }
        }
    }

    /// Retries with the same pinned helper and identity. Old relay sessions and pending
    /// introductions belong to the failed connection and are never carried into the new one.
    /// Commands fail promptly while disconnected, leaving the media redial loop in charge of
    /// retrying them. A host always republishes its CURRENT permissions, never a stale copy.
    async fn recover(&mut self, commands: &mut mpsc::Receiver<Command>) -> bool {
        self.connected.store(false, Ordering::Release);
        if let Some(client) = self.client.take() {
            client.close();
        }
        self.relayed = RelayPlaces::default();
        self.waiting.clear();
        self.dial_backs.abort_all();
        let mut gap = Duration::from_secs(1);
        loop {
            let delay = tokio::time::sleep(gap);
            tokio::pin!(delay);
            loop {
                tokio::select! {
                    () = &mut delay => break,
                    command = commands.recv() => match command {
                        Some(command) => disconnected(command),
                        None => return false,
                    },
                }
            }
            let node = self.node.clone();
            let server = self.server.clone();
            let (host, pass, room) = (self.host, self.pass, self.router.room_id);
            let attempt = async {
                let mut client =
                    RendezvousClient::connect(&node, &server.addrs, server.key).await?;
                let locals = veil_net::family::own_candidates(node.local_addr()?);
                client.register(room_tag(&room), host, pass, locals).await?;
                Ok::<_, veil_rendezvous::RendezvousError>(client)
            };
            tokio::pin!(attempt);
            let result = loop {
                tokio::select! {
                    result = &mut attempt => break result,
                    command = commands.recv() => match command {
                        Some(command) => disconnected(command),
                        None => return false,
                    },
                }
            };
            if let Ok(mut client) = result {
                // Registration alone must not make a host visible with stale permissions.
                let restored = if let Some(allowed) = self.allowed.as_mut() {
                    let now = allowed.borrow_and_update().clone();
                    client.allow(now.members, now.passes).await.is_ok()
                } else {
                    true
                };
                if restored {
                    let policy = self.consent.clone();
                    client.set_consent(Arc::new(move |from, kind| policy.agree(from, kind)));
                    self.client = Some(client);
                    self.connected.store(true, Ordering::Release);
                    return true;
                }
            }
            gap = (gap * 2).min(RECONNECT_MAX);
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Introduce(peer, reply) => {
                let result = match self.client.as_mut() {
                    Some(client) => client.introduce(peer).await.map_err(CallError::Rendezvous),
                    None => Err(CallError::Closed),
                };
                let _ = reply.send(result);
            }
            Command::Relay(peer, reply) => {
                let _ = reply.send(self.open_relay(peer).await);
            }
            Command::Recheck => {
                let now = Instant::now();
                self.waiting
                    .retain(|(.., at)| now.duration_since(*at) < WAIT);
                for (from, kind, at) in std::mem::take(&mut self.waiting) {
                    if self.consent.agree(&from, kind) {
                        if let Some(client) = self.client.as_mut() {
                            let _ = client.accept(from, kind).await;
                        }
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
            if let Some(client) = self.client.as_mut() {
                let _ = client.accept(from, kind).await;
            }
            return;
        }
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back((from, kind, Instant::now()));
    }

    async fn open_relay(&mut self, peer: PublicId) -> Result<Arc<Node>, CallError> {
        let client = self.client.as_mut().ok_or(CallError::Closed)?;
        let session = client
            .open_relay(peer)
            .await
            .map_err(CallError::Rendezvous)?;
        let socket = client
            .relay_socket(session)
            .map_err(CallError::Rendezvous)?;
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
                let Some(socket) = self
                    .client
                    .as_ref()
                    .and_then(|client| client.relay_socket(session).ok())
                else {
                    return;
                };
                let Ok(inner) = Node::bind_virtual(&self.identity, socket) else {
                    return;
                };
                let wait = self
                    .router
                    .clone()
                    .accept_relayed(Arc::new(inner), RELAY_WAIT);
                self.relayed.start(peer, wait);
            }
            _ => {}
        }
    }
}

async fn next_notice(client: &mut Option<RendezvousClient>) -> Option<ServerMsg> {
    match client.as_mut() {
        Some(client) => client.next_event().await,
        None => None,
    }
}

/// A caller waiting on a lost helper never has to wait through the backoff.
fn disconnected(command: Command) {
    match command {
        Command::Introduce(_, reply) => {
            let _ = reply.send(Err(CallError::Closed));
        }
        Command::Relay(_, reply) => {
            let _ = reply.send(Err(CallError::Closed));
        }
        Command::Recheck => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disconnected_requests_fail_without_waiting_for_backoff() {
        let (reply, answer) = oneshot::channel();
        disconnected(Command::Introduce(PublicId([1; 32]), reply));
        assert!(matches!(answer.await, Ok(Err(CallError::Closed))));
        let (reply, answer) = oneshot::channel();
        disconnected(Command::Relay(PublicId([1; 32]), reply));
        assert!(matches!(answer.await, Ok(Err(CallError::Closed))));
    }

    /// Real sockets: losing the helper is temporary and a host restores its NEW permission
    /// list. An old member must not regain permission from a snapshot taken before the loss.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    async fn helper_restart_restores_current_permissions_and_connectivity() {
        use veil_rendezvous::{Refusal, RendezvousError, RendezvousServer};
        let host_id = Identity::generate();
        let helper_id = Identity::generate();
        let removed = Identity::generate();
        let added = Identity::generate();
        let room = [41; 16];
        let bind = |id: &Identity, at| Node::bind(id, at).unwrap();
        let helper = bind(&helper_id, "127.0.0.1:0".parse().unwrap());
        let addr = helper.local_addr().unwrap();
        let server = RelayAddrs {
            key: helper.id(),
            addrs: vec![addr],
        };
        let running = tokio::spawn(RendezvousServer::new(helper).run());
        let node = Arc::new(bind(&host_id, "127.0.0.1:0".parse().unwrap()));
        let (allowed, rx) = watch::channel(Allowed {
            members: vec![removed.public()],
            passes: vec![],
        });
        let (media, _inbox) = mpsc::channel(1);
        let router = Router {
            room_id: room,
            host: None,
            media,
        };
        let consent = Arc::new(Consent {
            open: true,
            ..Consent::default()
        });
        let (presence, task) =
            Presence::start(&host_id, node, &server, router, consent, Seat::Host(rx))
                .await
                .unwrap();
        assert!(presence.is_connected());
        running.abort();
        let _ = running.await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while presence.is_connected() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detect helper loss");
        allowed
            .send(Allowed {
                members: vec![added.public()],
                passes: vec![],
            })
            .unwrap();
        // QUIC endpoint shutdown may retain its UDP socket briefly while draining.
        let helper = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match Node::bind(&helper_id, addr) {
                    Ok(node) => break node,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .expect("old helper releases its socket");
        let running = tokio::spawn(RendezvousServer::new(helper).run());
        tokio::time::timeout(Duration::from_secs(20), async {
            while !presence.is_connected() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("register after helper restart");
        for (identity, permitted) in [(&removed, false), (&added, true)] {
            let peer = bind(identity, "127.0.0.1:0".parse().unwrap());
            let mut client = RendezvousClient::connect(&peer, &[addr], helper_id.public())
                .await
                .unwrap();
            let result = client
                .register(room_tag(&room), host_id.public(), None, vec![])
                .await;
            if permitted {
                assert!(result.is_ok(), "latest allowed member is restored");
            } else {
                assert!(
                    matches!(result, Err(RendezvousError::Refused(Refusal::NotAllowed))),
                    "an old permission is not restored: {result:?}"
                );
            }
        }
        task.abort();
        let _ = task.await;
        assert!(
            !presence.is_connected(),
            "status clears when its actor is cancelled"
        );
        running.abort();
    }

    #[test]
    fn consent_follows_role_routes_and_relay_only() {
        let (known, stranger) = (PublicId([1; 32]), PublicId([2; 32]));
        let member = Consent::default();
        assert!(
            !member.allows(&known, Ask::Introduce),
            "nobody before routes"
        );
        member.expected.lock().map(|mut e| e.insert(known)).ok();
        assert!(member.allows(&known, Ask::Introduce));
        assert!(member.allows(&known, Ask::Relay));
        assert!(
            !member.allows(&stranger, Ask::Introduce),
            "an invite alone is not enough"
        );
        assert!(!member.allows(&stranger, Ask::Relay));
        member.relay_only.store(true, Ordering::Relaxed);
        assert!(
            !member.allows(&known, Ask::Introduce),
            "relay-only hides its address"
        );
        assert!(
            member.allows(&known, Ask::Relay),
            "but still takes relayed links"
        );

        let host = Consent {
            open: true,
            ..Consent::default()
        };
        assert!(
            host.allows(&stranger, Ask::Introduce),
            "joiners dial the host"
        );
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
        assert!(
            !host.may_dial_back(&peer, 1),
            "a relay is not an introduction"
        );
        assert!(host.agree(&peer, Ask::Introduce));
        assert!(host.may_dial_back(&peer, 1));

        // Agreed, then this node went relay-only before the notice came.
        let other = PublicId([2; 32]);
        assert!(host.agree(&other, Ask::Introduce));
        host.relay_only.store(true, Ordering::Relaxed);
        assert!(!host.may_dial_back(&other, 1));
        assert!(
            !host.agree(&other, Ask::Introduce),
            "and it agrees to no more"
        );
    }
}
