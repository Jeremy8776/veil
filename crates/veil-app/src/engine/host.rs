//! Hosting a call: the group, the lobby, the invite, and a plan for every key epoch.
//!
//! Purpose: starts the network node and, unless a `veil-node` rendezvous is configured, a
//! rendezvous of the host's own on a second port, so guests can find each other and fall
//! back to a relay. Then runs the host's loop: knocks become the lobby, the interface's
//! decisions go to `HostHandle::decide` and `remove`, chat goes to the session, and after
//! every `HostEvent::Keys` the host plans the epoch and publishes it with `publish_plan`.
//! If the person allowed it, the router is asked to open the call's port (`router.rs`);
//! once it does, its public address goes first in a fresh invite, and it is removed when
//! the call ends. With a public node, that address (or the first public address set by hand)
//! is then checked from outside (`reach.rs`).
//!
//! The forwarder for calls above 4 people is `VEIL_FORWARDER` if set. Otherwise, when the
//! host also runs its own rendezvous, it runs its own forwarder on a third port, reachable
//! at the same LAN address. With a `veil-node` rendezvous and no `VEIL_FORWARDER`, guests may
//! be anywhere, so the host offers no forwarder and the call stays a mesh.
//!
//! Invariants:
//! - Each invite has a one-time token, so a fresh one is made after every knock.
//! - The rendezvous sees addresses and a room hash only, never keys or media
//!   (`docs/ARCHITECTURE.md` section 2), and lives exactly as long as the call. The forwarder
//!   sees sealed frames only, and admits only devices on the host's signed roster ticket.
//!
//! shortcut: the host's own forwarder is reachable on its own network only, and the host's
//! uplink then carries every stream to every member. Set `VEIL_FORWARDER` to a public
//! `veil-node` for large calls across the internet.
//!
//! shortcut: applying routes waits for dials (a few seconds for a hole punch) and holds this
//! loop meanwhile, so media pauses briefly when someone joins. Every wait on a peer is bounded
//! (`CONNECT_TIMEOUT` to dial, then `veil_call::SEND_DEADLINE`), so a member that withholds
//! stream credit delays Remove by seconds at most (GHSA-37f5-22p2-q6hr). Move
//! `MediaSession::apply` to its own task when calls grow.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallNode, HostEvent, HostHandle};
use veil_forward::{ForwarderServer, Limits};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{RendezvousServer, room_tag};
use veil_security::{Identity, PublicId};

use super::plan::{NetConfig, advertised, lan_ip, plan_for, resolve};
use super::reach::Checker;
use super::router::{Event, Router};
use super::session::{Pipes, Session};
use super::view::{Knocker, Shared};
use super::{Command, Ids, short};
use crate::model::{Action, CallStatus, Reachable, RouterPort};

const INVITE_LIFETIME: Duration = Duration::from_secs(3_600);

pub async fn run(
    identity: Identity,
    config: NetConfig,
    your_name: &str,
    shared: Arc<Shared>,
    pipes: Pipes,
    mut commands: UnboundedReceiver<Command>,
) -> Result<(), String> {
    let me = identity.public();
    // A fresh key's public half is 32 random-looking bytes; its first 16 make a room id.
    let room_id: [u8; 16] = Identity::generate().public().0[..16]
        .try_into()
        .map_err(|_| "room id")?;
    let bind = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), config.port);
    let node = Node::bind(&identity, bind).map_err(|e| format!("Cannot open the network: {e}"))?;
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let lan = lan_ip();
    let configured = resolve(config.rendezvous.as_ref(), "rendezvous").await?;
    let rendezvous = match configured {
        Some(addr) => addr,
        None => own_server(lan, Own::Rendezvous, &identity, room_id)?,
    };
    let forwarder = match (
        resolve(config.forwarder.as_ref(), "forwarder").await?,
        configured,
    ) {
        (Some(addr), _) => Some(addr),
        (None, None) => Some(own_server(lan, Own::Forwarder, &identity, room_id)?),
        (None, Some(_)) => None,
    };
    let (host, mut events) = HostHandle::start(&identity, room_id).map_err(|e| e.to_string())?;
    let mut call = CallNode::start(&identity, node, room_id, Some(rendezvous), host.clone()).await;
    if !call.has_rendezvous() {
        crate::note!("rendezvous {rendezvous} did not answer; direct links only");
    }
    let mut addrs = advertised(&config, lan, port);
    let invite = new_invite(&host, &addrs, &rendezvous).await?;
    let mut router = Router::start(config.open_port, lan, port);
    let mut router_addr: Option<SocketAddr> = None;
    // Only a public node can dial in from outside; the host's own rendezvous cannot.
    let mut reach = Checker::default();
    let checking = config
        .advertise
        .first()
        .is_some_and(|at| check(&mut reach, &call, configured, *at));
    shared.update(|view| {
        view.invite = invite;
        view.status = CallStatus::Live;
        if router.is_some() {
            view.router = RouterPort::Opening;
        }
        if checking {
            view.reach = Reachable::Checking;
        }
    });
    let media = call.media().map_err(|e| e.to_string())?;
    let mut session = Session::new(media, &identity, me, your_name, shared.clone(), pipes);
    let mut knocks: Vec<(u32, PublicId)> = Vec::new();
    let result = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Leave) => break Ok(()),
                Some(Command::Act(Action::Chat(text))) => session.send_chat(&text).await,
                Some(Command::Act(action)) => act(&host, &session.ids, &mut knocks, action).await,
            },
            event = events.recv() => match event {
                None => break Err("The call stopped unexpectedly.".into()),
                Some(HostEvent::Knock(device)) => {
                    let id = session.ids.id(device);
                    knocks.push((id, device));
                    let knocker = Knocker {
                        id,
                        device,
                        name: session.name_for(&device),
                        safety: session.safety_with(&device),
                        since: Instant::now(),
                    };
                    // The knock used the invite's token: give out a fresh one.
                    let invite = new_invite(&host, &addrs, &rendezvous).await.ok();
                    shared.update(|view| {
                        view.lobby.push(knocker);
                        if let Some(invite) = invite {
                            view.invite = invite;
                        }
                    });
                }
                // Joins and leaves go to the log, so a call across networks shows when the
                // host heard of each.
                Some(HostEvent::Joined { device, .. }) => {
                    crate::note!("call: {} joined", short(&device));
                    let id = session.ids.id(device);
                    knocks.retain(|(_, d)| *d != device);
                    shared.update(|view| view.lobby.retain(|k| k.id != id));
                }
                Some(HostEvent::Left(device)) => {
                    crate::note!("call: {} left", short(&device));
                    let id = session.ids.id(device);
                    knocks.retain(|(_, d)| *d != device);
                    shared.update(|view| view.lobby.retain(|k| k.id != id));
                }
                Some(HostEvent::Keys(keys)) => {
                    let roster = keys.roster.clone();
                    session.on_keys(keys);
                    // A plan that fails leaves the old routes standing; the call goes on.
                    if roster.len() > 1 {
                        match plan_for(&roster, me, &addrs, forwarder) {
                            Ok((plan, directory)) => match host.publish_plan(plan, directory).await {
                                Ok(routes) => session.on_routes(routes).await,
                                Err(e) => crate::note!("cannot publish the plan: {e}"),
                            },
                            Err(e) => crate::note!("{e}"),
                        }
                    }
                }
                Some(HostEvent::Error(e)) => crate::note!("group error: {e}"),
            },
            () = session.pump() => {}
            event = next_router(&mut router) => {
                let (state, open) = match event {
                    Event::Open(addr) => (RouterPort::Open(addr.to_string()), Some(addr)),
                    Event::Failed(guide) => (RouterPort::Failed(guide), None),
                };
                // The router's address goes first in every invite from now on.
                if open != router_addr {
                    addrs.retain(|a| Some(*a) != router_addr);
                    if let Some(addr) = open {
                        addrs.retain(|a| *a != addr);
                        addrs.insert(0, addr);
                        addrs.truncate(veil_security::invite::MAX_ADDRS);
                    }
                    router_addr = open;
                }
                let checking = open.is_some_and(|at| check(&mut reach, &call, configured, at));
                let invite = new_invite(&host, &addrs, &rendezvous).await.ok();
                shared.update(|view| {
                    view.router = state;
                    if checking {
                        view.reach = Reachable::Checking;
                    }
                    if let Some(invite) = invite {
                        view.invite = invite;
                    }
                });
            }
            verdict = reach.next() => shared.update(|view| view.reach = verdict),
        }
    };
    if let Some(router) = router {
        router.close().await;
    }
    result
}

/// Asks the public node `server`, if there is one, to dial `at` from outside. Returns whether
/// a check started.
fn check(reach: &mut Checker, call: &CallNode, server: Option<NodeAddr>, at: SocketAddr) -> bool {
    let Some(server) = server else {
        return false;
    };
    reach.start(at, call.check_reach(server, at.port()));
    true
}

/// The router's next change, or never when no port was asked for.
async fn next_router(router: &mut Option<Router>) -> Event {
    match router {
        Some(router) => router.next().await,
        None => std::future::pending().await,
    }
}

async fn act(host: &HostHandle, ids: &Ids, knocks: &mut Vec<(u32, PublicId)>, action: Action) {
    let result = match action {
        Action::Admit(id) | Action::Decline(id) => {
            let Some(index) = knocks.iter().position(|(k, _)| *k == id) else {
                return;
            };
            let (_, device) = knocks.remove(index);
            host.decide(device, matches!(action, Action::Admit(_)))
                .await
        }
        Action::Remove(id) => match ids.device(id) {
            Some(device) => host.remove(device).await,
            None => return,
        },
        // The session sends chat (see `run`); the engine saves verified keys.
        Action::Chat(_) | Action::Verify(..) => return,
    };
    if let Err(e) = result {
        crate::note!("{action:?} failed: {e}");
    }
}

#[derive(Clone, Copy)]
enum Own {
    /// Rendezvous and relay.
    Rendezvous,
    Forwarder,
}

/// Runs a rendezvous (with relay) or a forwarder on a port of its own, with a key of its own,
/// reachable at this machine's LAN address (or this machine only, without one). It lives as
/// long as the call's runtime.
/// Starts a rendezvous or forwarder for this call only: it serves `host`'s room `room_id` and
/// refuses everyone else's, so strangers who find its port cannot use this upload.
fn own_server(
    lan: Option<IpAddr>,
    kind: Own,
    host: &Identity,
    room_id: [u8; 16],
) -> Result<NodeAddr, String> {
    let what = match kind {
        Own::Rendezvous => "rendezvous",
        Own::Forwarder => "forwarder",
    };
    let identity = Identity::generate();
    let node = Node::bind(&identity, SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
        .map_err(|e| format!("Cannot open the {what}: {e}"))?;
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let addr = NodeAddr {
        key: node.id(),
        addr: SocketAddr::new(lan.unwrap_or(Ipv4Addr::LOCALHOST.into()), port),
    };
    match kind {
        Own::Rendezvous => drop(tokio::spawn(
            RendezvousServer::new(node)
                .only_room(room_tag(&room_id), host.public())
                .run(),
        )),
        Own::Forwarder => drop(tokio::spawn(
            ForwarderServer::new(node, Limits::default())
                .only_host(host.public())
                .run(),
        )),
    }
    Ok(addr)
}

async fn new_invite(
    host: &HostHandle,
    addrs: &[SocketAddr],
    rendezvous: &NodeAddr,
) -> Result<String, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    host.invite(
        addrs.iter().map(ToString::to_string).collect(),
        Some(rendezvous.to_string()),
        now + INVITE_LIFETIME.as_secs(),
    )
    .await
    .map_err(|e| format!("Cannot make an invite: {e}"))
}
