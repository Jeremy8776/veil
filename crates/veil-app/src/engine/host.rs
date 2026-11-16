//! Hosting a call: the group, the lobby, the invite, and a plan for every key epoch.
//!
//! Purpose: starts the network node and, unless a `veil-node` rendezvous is configured, a
//! rendezvous of the host's own on a second port, so guests can find each other and fall
//! back to a relay. Then runs the host's loop: knocks become the lobby, the interface's
//! decisions go to `HostHandle::decide` and `remove`, chat goes to the session, and after
//! every `HostEvent::Keys` the host plans the epoch and publishes it with `publish_plan`.
//!
//! Invariants:
//! - Each invite has a one-time token, so a fresh one is made after every knock.
//! - The rendezvous sees addresses and a room hash only, never keys or media
//!   (`docs/ARCHITECTURE.md` section 2), and lives exactly as long as the call.
//!
//! shortcut: applying routes waits for dials (a few seconds for a hole punch) and holds this
//! loop meanwhile, so media pauses briefly when someone joins. Move `MediaSession::apply` to
//! its own task when calls grow.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallNode, HostEvent, HostHandle};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::RendezvousServer;
use veil_security::{Identity, PublicId};

use super::plan::{NetConfig, advertised, lan_ip, plan_for};
use super::session::{Pipes, Session};
use super::view::{Knocker, Shared};
use super::{Command, Ids};
use crate::model::{Action, CallStatus};

const INVITE_LIFETIME: Duration = Duration::from_secs(3_600);

pub async fn run(
    config: NetConfig,
    your_name: &str,
    shared: Arc<Shared>,
    pipes: Pipes,
    mut commands: UnboundedReceiver<Command>,
) -> Result<(), String> {
    let identity = Identity::generate();
    let me = identity.public();
    // A fresh key's public half is 32 random-looking bytes; its first 16 make a room id.
    let room_id: [u8; 16] = Identity::generate().public().0[..16]
        .try_into()
        .map_err(|_| "room id")?;
    let bind = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), config.port);
    let node = Node::bind(&identity, bind).map_err(|e| format!("Cannot open the network: {e}"))?;
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let lan = lan_ip();
    let rendezvous = match config.rendezvous {
        Some(addr) => addr,
        None => own_rendezvous(lan)?,
    };
    let (host, mut events) = HostHandle::start(&identity, room_id).map_err(|e| e.to_string())?;
    let mut call = CallNode::start(
        &identity,
        node,
        room_id,
        Some(rendezvous),
        Some(host.clone()),
    )
    .await;
    if !call.has_rendezvous() {
        eprintln!("veil-app: rendezvous {rendezvous} did not answer; direct links only");
    }
    let addrs = advertised(&config, lan, port);
    let invite = new_invite(&host, &addrs, &rendezvous).await?;
    shared.update(|view| {
        view.invite = invite;
        view.status = CallStatus::Live;
    });
    let media = call.media().map_err(|e| e.to_string())?;
    let mut session = Session::new(media, me, me, your_name, shared.clone(), pipes);
    let mut knocks: Vec<(u32, PublicId)> = Vec::new();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Leave) => return Ok(()),
                Some(Command::Act(Action::Chat(text))) => session.send_chat(&text).await,
                Some(Command::Act(action)) => act(&host, &session.ids, &mut knocks, action).await,
            },
            event = events.recv() => match event {
                None => return Err("The call stopped unexpectedly.".into()),
                Some(HostEvent::Knock(device)) => {
                    let id = session.ids.id(device);
                    knocks.push((id, device));
                    let knocker = Knocker {
                        id,
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
                Some(HostEvent::Joined { device, .. } | HostEvent::Left(device)) => {
                    let id = session.ids.id(device);
                    knocks.retain(|(_, d)| *d != device);
                    shared.update(|view| view.lobby.retain(|k| k.id != id));
                }
                Some(HostEvent::Keys(keys)) => {
                    let roster = keys.roster.clone();
                    session.on_keys(keys);
                    if roster.len() > 1 {
                        let (plan, directory) = plan_for(&roster, me, &addrs)?;
                        match host.publish_plan(plan, directory).await {
                            Ok(routes) => session.on_routes(routes).await,
                            Err(e) => eprintln!("veil-app: cannot publish the plan: {e}"),
                        }
                    }
                }
                Some(HostEvent::Error(e)) => eprintln!("veil-app: group error: {e}"),
            },
            () = session.pump() => {}
        }
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
        // The session sends chat; see `run`.
        Action::Chat(_) => return,
    };
    if let Err(e) = result {
        eprintln!("veil-app: {action:?} failed: {e}");
    }
}

/// Runs a rendezvous and relay on a port of its own, reachable at this machine's LAN
/// address (or this machine only, without one).
fn own_rendezvous(lan: Option<IpAddr>) -> Result<NodeAddr, String> {
    let identity = Identity::generate();
    let node = Node::bind(&identity, SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
        .map_err(|e| format!("Cannot open the rendezvous: {e}"))?;
    let server = RendezvousServer::new(node);
    let port = server.local_addr().map_err(|e| e.to_string())?.port();
    let addr = NodeAddr {
        key: server.id(),
        addr: SocketAddr::new(lan.unwrap_or(Ipv4Addr::LOCALHOST.into()), port),
    };
    tokio::spawn(server.run());
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
