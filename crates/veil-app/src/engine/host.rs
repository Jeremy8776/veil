//! Hosting a call: the group, the lobby, the invite, and a plan for every key epoch.
//!
//! Purpose: starts the network node and, unless a `veil-node` rendezvous is configured, a
//! rendezvous of the host's own on a second port, so guests can find each other and fall
//! back to a relay. Then runs the host's loop: knocks become the lobby, the interface's
//! decisions go to `HostHandle::decide` and `remove`, chat goes to the session, and after
//! every `HostEvent::Keys` the host plans the epoch and publishes it with `publish_plan`.
//! If the person allowed it, the router is asked to open the call's port (`router.rs`);
//! once it does, its public address goes first in a fresh invite, and it is removed when
//! the call ends. The host's own rendezvous gets its port opened the same way, and the invite
//! then names it at both its public and LAN addresses (`veil_call::RelayAddrs`). With a public
//! node, that address (or the first public address set by hand) is then checked from outside
//! (`reach.rs`).
//!
//! The call's socket is dual-stack where the system allows it. A public IPv6 address goes in
//! every invite and to the rendezvous, guests dial it first, and, with port opening on, the
//! router is asked for an IPv6 pinhole (`pinhole.rs`). If the public node has an IPv6 address
//! too, the IPv6 address is checked from outside as well.
//!
//! The forwarder for calls above 4 people is `VEIL_FORWARDER` if set. Otherwise, when the
//! host also runs its own rendezvous, it runs its own forwarder on a third port, reachable
//! at the same LAN address. With a `veil-node` rendezvous and no `VEIL_FORWARDER`, guests may
//! be anywhere, so the host offers no forwarder and the call stays a mesh.
//!
//! Invariants:
//! - Each invite has a one-time token, so a fresh one is made after every knock.
//! - The link shown is short when the rendezvous can hold it and reaches as far as the call
//!   (`invites::can_shorten`). The full link is always kept beside it.
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
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallNode, HostEvent, HostHandle, RelayAddrs};
use veil_forward::{ForwarderServer, Limits};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{RendezvousServer, room_tag};
use veil_security::{Identity, PublicId};

use super::plan::{NetConfig, advertised, lan_ip, plan_for, resolve};
use super::pinhole::V6Port;
use super::reach::Reachability;
use super::router::{Event, PortStatus, Router};
use super::session::{Pipes, Session};
use super::view::{Knocker, Shared};
use super::{Command, Ids, invites, short};
use crate::model::{Action, CallStatus};

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
    let node = Node::bind_any(&identity, config.port)
        .map_err(|e| format!("Cannot open the network: {e}"))?;
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let lan = lan_ip();
    let v6 = if node.ipv6() { veil_net::family::global_ipv6() } else { None };
    crate::note!("network: port {port}, IPv6 {}", v6.map_or_else(|| "none".into(), |ip| ip.to_string()));
    let configured = resolve(config.rendezvous.as_ref(), "rendezvous").await?;
    // The public node's IPv6 address, for checking ours from outside.
    let configured6 = match (&config.rendezvous, v6) {
        (Some(node), Some(_)) => node.resolve_ipv6().await,
        _ => None,
    };
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
    let mut addrs = advertised(&config, v6, lan, port);
    let mut until_end = false;
    let mut relay = RelayAddrs::from(rendezvous);
    let mut expires = new_invite(&host, &call, &addrs, &relay, until_end, &shared).await;
    if expires == 0 {
        return Err("Cannot make an invite.".into());
    }
    let mut router = Router::start(config.open_port, lan, port);
    // Guests reach each other by punching or relaying through the rendezvous, so the host's
    // own one needs its port open too, or guests on the internet see the host but not each
    // other. A rendezvous set in Settings is already public.
    let mut relay_router = match configured {
        None => Router::start(config.open_port, lan, rendezvous.addr.port()),
        Some(_) => None,
    };
    let mut ports = PortStatus::new(router.as_ref(), relay_router.as_ref());
    let mut v6_port = V6Port::start(config.open_port, v6, port);
    let mut router_addr: Option<SocketAddr> = None;
    // Only a public node can dial in from outside; the host's own rendezvous cannot.
    let mut reach = Reachability::default();
    let mut checking = config
        .advertise
        .first()
        .is_some_and(|at| check(&mut reach, &call, configured, *at));
    // Without a pinhole to wait for, the IPv6 address is checked at once.
    let at6 = v6.map(|ip| SocketAddr::new(ip.into(), port));
    if v6_port.is_none() {
        checking |= at6.is_some_and(|at| check(&mut reach, &call, configured6, at));
    }
    shared.update(|view| {
        view.status = CallStatus::Live;
        view.router = ports.view();
        if checking {
            view.reach = reach.shown();
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
                Some(Command::Act(Action::Hand(raised))) => session.set_hand(raised).await,
                // A fresh link with the chosen lifetime; one already sent keeps its own.
                Some(Command::Act(Action::InviteUntilEnd(chosen))) => {
                    until_end = chosen;
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, &shared).await;
                }
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
                    shared.update(|view| view.lobby.push(knocker));
                    // The knock used the invite's token: give out a fresh one.
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, &shared).await;
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
                let open = match &event {
                    Event::Open(addr) => Some(*addr),
                    Event::Failed(_) => None,
                };
                ports.call_changed(event);
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
                shared.update(|view| {
                    view.router = ports.view();
                    if checking {
                        view.reach = reach.shown();
                    }
                });
                if ports.settled() {
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, &shared).await;
                }
            }
            event = next_router(&mut relay_router) => {
                // The public address first, for guests on the internet; the LAN address stays
                // for guests on this network, since many routers do not loop back.
                let public = match &event {
                    Event::Open(addr) => Some(*addr),
                    Event::Failed(_) => None,
                };
                ports.rendezvous_changed(event);
                if let Some(addr) = public {
                    crate::note!("rendezvous: guests on the internet reach it at {addr}");
                }
                relay.addrs = public.into_iter().chain([rendezvous.addr]).collect();
                shared.update(|view| view.router = ports.view());
                if ports.settled() {
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, &shared).await;
                }
            }
            // The link shown always works: one that ran out unused is replaced.
            () = until(expires) => {
                expires = new_invite(&host, &call, &addrs, &relay, until_end, &shared).await;
            }
            // Whether or not the pinhole opened, the IPv6 address can be checked now.
            _ = next_v6(&mut v6_port) => {
                if at6.is_some_and(|at| check(&mut reach, &call, configured6, at)) {
                    shared.update(|view| view.reach = reach.shown());
                }
            }
            verdict = reach.next() => shared.update(|view| view.reach = verdict),
        }
    };
    let close_v6 = async {
        if let Some(v6_port) = v6_port {
            v6_port.close().await;
        }
    };
    // Together, because leaving waits about a second and each close may take 0.8 s of it.
    tokio::join!(close_router(router), close_router(relay_router), close_v6);
    result
}

async fn close_router(router: Option<Router>) {
    if let Some(router) = router {
        router.close().await;
    }
}

/// Asks the public node `server`, if there is one, to dial `at` from outside. Returns whether
/// a check started.
fn check(
    reach: &mut Reachability,
    call: &CallNode,
    server: Option<NodeAddr>,
    at: SocketAddr,
) -> bool {
    let Some(server) = server else {
        return false;
    };
    reach.start(at, call.check_reach(server, at.port()));
    true
}

/// Whether the IPv6 pinhole opened, once; never when none was asked for.
async fn next_v6(v6_port: &mut Option<V6Port>) -> bool {
    match v6_port {
        Some(v6_port) => v6_port.next().await,
        None => std::future::pending().await,
    }
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
        Action::Chat(_) | Action::Hand(_) | Action::Verify(..) | Action::InviteUntilEnd(_) => return,
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

/// The link to show for `full`: a short one when that works as well, otherwise `full` itself.
async fn shortened(
    call: &CallNode,
    addrs: &[SocketAddr],
    relay: &RelayAddrs,
    full: &str,
) -> String {
    if !invites::can_shorten(addrs, &relay.addrs) {
        return full.to_string();
    }
    call.shorten_invite(relay, full).await.unwrap_or_else(|e| {
        crate::note!("no short invite link, using the full one: {e}");
        full.to_string()
    })
}

/// Makes a fresh invite link that works for an hour or `until_end`, and shows it. Returns
/// when to look again: the link's expiry, or a minute from now if no link could be made
/// (logged), so a failure is retried without spinning.
async fn new_invite(
    host: &HostHandle,
    call: &CallNode,
    addrs: &[SocketAddr],
    relay: &RelayAddrs,
    until_end: bool,
    shared: &Shared,
) -> u64 {
    let now = invites::now_unix();
    let expires = invites::expiry(until_end, now);
    let listed = addrs.iter().map(ToString::to_string).collect();
    match host.invite(listed, Some(relay.to_string()), expires).await {
        Ok(full) => {
            let link = shortened(call, addrs, relay, &full).await;
            shared.update(|view| {
                view.invite = link;
                view.invite_full = full;
                view.invite_expires = expires;
                view.invite_until_end = until_end;
            });
            expires
        }
        Err(e) => {
            crate::note!("cannot make an invite: {e}");
            now + 60
        }
    }
}

/// Waits until the Unix time `at`.
async fn until(at: u64) {
    let now = invites::now_unix();
    if !invites::expired(at, now) {
        tokio::time::sleep(Duration::from_secs(at - now)).await;
    }
}
