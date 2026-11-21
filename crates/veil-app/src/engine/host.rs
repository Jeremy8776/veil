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
//! A forwarder is proposed only after a mesh plan is in place. Every current member checks
//! its pinned endpoint and signed roster ticket without joining. Only unanimous, timely
//! replies for the current request permit publishing the forwarded plan. A remote guest
//! unable to reach the host's LAN forwarder therefore keeps the call in its mesh.
//!
//! shortcut: applying routes waits for dials (a few seconds for a hole punch) and holds this
//! loop meanwhile, so media pauses briefly when someone joins. Every wait on a peer is bounded
//! (`CONNECT_TIMEOUT` to dial, then `veil_call::SEND_DEADLINE`), so a member that withholds
//! stream credit delays Remove by seconds at most (GHSA-37f5-22p2-q6hr). Move
//! `MediaSession::apply` to its own task when calls grow.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallNode, HostEvent, HostHandle, RelayAddrs};
use veil_forward::{ForwarderServer, Limits};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{Refusal, RendezvousServer, room_tag};
use veil_security::{Identity, PublicId};

mod forwarder_check;

use forwarder_check::{checked_plan, finish_checked_plan, own_check};
use super::pinhole::V6Port;
use super::plan::{NetConfig, advertised, lan_ip, resolve};
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
    let config = config.enforce_policy();
    let me = identity.public();
    // A fresh key's public half is 32 random-looking bytes; its first 16 make a room id.
    let room_id: [u8; 16] = Identity::generate().public().0[..16]
        .try_into()
        .map_err(|_| "room id")?;
    let node = if config.local_only {
        Node::bind_local(&identity, config.port)
    } else {
        Node::bind_any(&identity, config.port)
    }
    .map_err(|e| format!("Cannot open the network: {e}"))?;
    node.set_local_only(config.local_only);
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let lan = lan_ip();
    let v6 = if !config.local_only && node.ipv6() {
        veil_net::family::global_ipv6()
    } else {
        None
    };
    crate::note!(
        "network: port {port}, IPv6 {}",
        v6.map_or_else(|| "none".into(), |ip| ip.to_string())
    );
    // A server set in Settings that cannot be found stops the call, as before, rather than
    // quietly hosting on this network only. Local only clears these first, so a local call
    // never waits on them.
    let configured = resolve(config.rendezvous.as_ref(), "rendezvous").await?;
    // The public node's IPv6 address, for checking ours from outside.
    let configured6 = match (&config.rendezvous, v6) {
        (Some(node), Some(_)) => node.resolve_ipv6().await,
        _ => None,
    };
    let rendezvous = match configured {
        Some(addr) => addr,
        None => own_server(lan, Own::Rendezvous, &identity, room_id, config.local_only)?,
    };
    let forwarder = match (
        resolve(config.forwarder.as_ref(), "forwarder").await?,
        configured,
    ) {
        (Some(addr), _) => Some(addr),
        (None, None) => Some(own_server(
            lan,
            Own::Forwarder,
            &identity,
            room_id,
            config.local_only,
        )?),
        (None, Some(_)) => None,
    };
    let (host, mut events) = HostHandle::start(&identity, room_id).map_err(|e| e.to_string())?;
    let mut call = CallNode::start(&identity, node, room_id, Some(rendezvous), host.clone()).await;
    if configured.is_some() && call.rendezvous_refusal() == Some(Refusal::NotAllowed) {
        // A node that lets only chosen device keys host. Without it nobody could find the
        // call, so stop here and say what to do rather than run a call nobody can join.
        return Err(String::from(
            "The rendezvous server in Settings > Connection does not let this device host. \
             Send its owner your device key (Copy device key on Home).",
        ));
    }
    if !call.has_rendezvous() {
        crate::note!("rendezvous {rendezvous} did not answer; direct links only");
    }
    let mut addrs = advertised(&config, v6, lan, port);
    publish_addresses(&host, &addrs).await;
    let mut until_end = false;
    let mut relay = RelayAddrs::from(rendezvous);
    if configured.is_none() {
        relay.addrs = hosted_addresses(rendezvous, None, config.local_only);
    }
    let mut expires = new_invite(
        &host,
        &call,
        &addrs,
        &relay,
        until_end,
        config.local_only,
        &shared,
    )
    .await;
    if expires == 0 {
        return Err("Cannot make an invite.".into());
    }
    // `enforce_policy` already clears discovery outside Local only; this is the second lock.
    let _nearby = if config.nearby_discovery && config.local_only {
        match crate::nearby::advertise(shared.clone()) {
            Ok(advertiser) => Some(advertiser),
            Err(e) => {
                crate::note!("nearby discovery could not start: {e}");
                None
            }
        }
    } else {
        None
    };
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
    let mut roster = Vec::new();
    let mut peer_addrs: HashMap<PublicId, Vec<SocketAddr>> = HashMap::new();
    let mut pending_plan = None;
    let mut request = 0u64;
    // Whether members' media currently runs through the forwarder, and whether an address
    // change is waiting for the next tick. Address changes replan at most once a tick.
    let mut forwarded = false;
    let mut replan = false;
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Leave) => break Ok(()),
                Some(Command::Act(said @ (Action::Chat(..) | Action::React(..)))) => {
                    session.say(&said).await;
                }
                Some(Command::Act(Action::Hand(raised))) => session.set_hand(raised).await,
                // A fresh link with the chosen lifetime; one already sent keeps its own.
                Some(Command::Act(Action::InviteUntilEnd(chosen))) => {
                    until_end = chosen;
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, config.local_only, &shared).await;
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
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, config.local_only, &shared).await;
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
                    peer_addrs.remove(&device);
                }
                Some(HostEvent::Keys(keys)) => {
                    roster = keys.roster.clone();
                    session.on_keys(keys);
                    peer_addrs.retain(|device, _| roster.iter().any(|(_, d)| d == device));
                    replan = false;
                    pending_plan = checked_plan(&host, &call, &mut session, &roster, me,
                        &addrs, &peer_addrs, forwarder, &mut request, &mut forwarded).await;
                }
                Some(HostEvent::Addresses { device, addrs: candidate }) => {
                    // Only current members' hints are kept, so this map is bounded by the roster.
                    if roster.iter().any(|(_, d)| *d == device)
                        && peer_addrs.get(&device) != Some(&candidate)
                    {
                        peer_addrs.insert(device, candidate);
                        replan = true;
                    }
                }
                Some(HostEvent::PlanChecked { device, request, ok }) => {
                    if let Some(pending) = pending_plan.as_mut() {
                        pending.checks.report(device, request, ok);
                    }
                    finish_checked_plan(&mut pending_plan, &host, &mut session, &mut forwarded).await;
                }
                Some(HostEvent::Refused(device, reason)) => {
                    crate::note!("call: turned {} away: {reason:?}", short(&device));
                }
                Some(HostEvent::Error(e)) => crate::note!("group error: {e}"),
            },
            (request, ok) = own_check(&mut pending_plan) => {
                if let Some(pending) = pending_plan.as_mut() {
                    pending.own.take();
                    pending.checks.report(me, request, ok);
                }
                finish_checked_plan(&mut pending_plan, &host, &mut session, &mut forwarded).await;
            }
            _ = refresh.tick() => {
                finish_checked_plan(&mut pending_plan, &host, &mut session, &mut forwarded).await;
                if replan {
                    replan = false;
                    pending_plan = checked_plan(&host, &call, &mut session, &roster, me,
                        &addrs, &peer_addrs, forwarder, &mut request, &mut forwarded).await;
                }
                let current_v6 = if config.local_only { None } else { veil_net::family::global_ipv6() };
                let mut current = advertised(&config, current_v6, lan_ip(), port);
                if let Some(mapped) = router_addr {
                    current.retain(|at| *at != mapped);
                    current.insert(0, mapped);
                    current.truncate(veil_security::invite::MAX_ADDRS);
                }
                if current != addrs {
                    addrs = current;
                    publish_addresses(&host, &addrs).await;
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, config.local_only, &shared).await;
                    pending_plan = checked_plan(&host, &call, &mut session, &roster, me,
                        &addrs, &peer_addrs, forwarder, &mut request, &mut forwarded).await;
                }
            }
            () = session.pump() => {}
            event = next_router(&mut router) => {
                let open = match &event {
                    Event::Open(addr) => Some(*addr),
                    Event::Failed(_) => None,
                };
                ports.call_changed(event);
                // The router's address goes first in every invite from now on.
                let changed = open != router_addr;
                if changed {
                    addrs.retain(|a| Some(*a) != router_addr);
                    if let Some(addr) = open {
                        addrs.retain(|a| *a != addr);
                        addrs.insert(0, addr);
                        addrs.truncate(veil_security::invite::MAX_ADDRS);
                    }
                    router_addr = open;
                }
                if changed {
                    publish_addresses(&host, &addrs).await;
                    pending_plan = checked_plan(&host, &call, &mut session, &roster, me,
                        &addrs, &peer_addrs, forwarder, &mut request, &mut forwarded).await;
                }
                let checking = open.is_some_and(|at| check(&mut reach, &call, configured, at));
                shared.update(|view| {
                    view.router = ports.view();
                    if checking {
                        view.reach = reach.shown();
                    }
                });
                if ports.settled() {
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, config.local_only, &shared).await;
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
                relay.addrs = hosted_addresses(rendezvous, public, config.local_only);
                shared.update(|view| view.router = ports.view());
                if ports.settled() {
                    expires = new_invite(&host, &call, &addrs, &relay, until_end, config.local_only, &shared).await;
                }
            }
            // The link shown always works: one that ran out unused is replaced.
            () = until(expires) => {
                expires = new_invite(&host, &call, &addrs, &relay, until_end, config.local_only, &shared).await;
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

async fn publish_addresses(host: &HostHandle, addrs: &[SocketAddr]) {
    if let Err(e) = host.set_addresses(addrs.to_vec()).await {
        crate::note!("cannot update host connection addresses: {e}");
    }
}

/// Where guests reach the host's own helper. In a Local only call it listens on every
/// interface, so its local scope addresses are listed too (IPv6-only LANs need them).
/// Otherwise only the router's public mapping and the LAN address are given, as before.
fn hosted_addresses(node: NodeAddr, public: Option<SocketAddr>, local_only: bool) -> Vec<SocketAddr> {
    let extra = if local_only {
        veil_net::local::local_candidates(node.addr.port())
    } else {
        Vec::new()
    };
    let mut addrs: Vec<_> = public.into_iter().chain([node.addr]).chain(extra).collect();
    let mut seen = HashSet::new();
    addrs.retain(|at| seen.insert(*at));
    addrs.truncate(veil_call::relay_field::MAX_RELAY_ADDRS);
    addrs
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
        Action::Chat(..)
        | Action::React(..)
        | Action::Hand(_)
        | Action::Verify(..)
        | Action::InviteUntilEnd(_) => {
            return;
        }
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
    local_only: bool,
) -> Result<NodeAddr, String> {
    let what = match kind {
        Own::Rendezvous => "rendezvous",
        Own::Forwarder => "forwarder",
    };
    let identity = Identity::generate();
    // Outside Local only the helper stays IPv4, as before: dual stack would put it on the
    // host's global IPv6 address, reachable from the whole internet.
    let node = if local_only {
        Node::bind_local(&identity, 0)
    } else {
        Node::bind(&identity, SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
    }
    .map_err(|e| format!("Cannot open the {what}: {e}"))?;
    node.set_local_only(local_only);
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let candidates = veil_net::local::local_candidates(port);
    let local_addr = candidates
        .iter()
        .find(|at| Some(at.ip()) == lan)
        .or_else(|| if local_only { candidates.first() } else { None })
        .copied()
        .unwrap_or_else(|| SocketAddr::new(lan.unwrap_or(Ipv4Addr::LOCALHOST.into()), port));
    let addr = NodeAddr {
        key: node.id(),
        addr: local_addr,
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
#[allow(clippy::too_many_arguments)]
async fn new_invite(
    host: &HostHandle,
    call: &CallNode,
    addrs: &[SocketAddr],
    relay: &RelayAddrs,
    until_end: bool,
    local_only: bool,
    shared: &Shared,
) -> u64 {
    let now = invites::now_unix();
    let expires = invites::expiry(until_end, now);
    let listed = addrs.iter().map(ToString::to_string).collect();
    match host.invite(listed, Some(relay.to_string()), expires).await {
        Ok(full) => {
            let link = if local_only {
                full.clone()
            } else {
                shortened(call, addrs, relay, &full).await
            };
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
