//! Media links from the topology plan, over real QUIC links on localhost: mesh with punched
//! dials, forwarded through `veil-forward`, and the relay for both joining and media. Routes
//! come from the host's signed plan.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{
    AppMessage, CallError, CallNode, Directory, EpochKeys, HostEvent, HostHandle, KeyRing, Media,
    MediaSession, MemberEvent, MemberHandle, Path, Peer, Routes, join,
};
use veil_forward::{ForwarderServer, Limits};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::RendezvousServer;
use veil_rendezvous::socket::RELAY_PEER;
use veil_security::invite::Invite;
use veil_security::{Identity, SecurityError};
use veil_topology::{NodeId, NodeKind, Policy, plan};

const ROOM: [u8; 16] = [6; 16];
const FORWARDER_ID: NodeId = NodeId(1_000);

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f).await.expect("timed out")
}

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn start_rendezvous() -> NodeAddr {
    let server = RendezvousServer::new(Node::bind(&Identity::generate(), localhost()).unwrap());
    let addr = NodeAddr { key: server.id(), addr: server.local_addr().unwrap() };
    tokio::spawn(server.run());
    addr
}

fn start_forwarder() -> NodeAddr {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = ForwarderServer::new(node, Limits::default());
    let addr = NodeAddr { key: server.id(), addr: server.local_addr().unwrap() };
    tokio::spawn(server.run());
    addr
}

/// One participant: its node, its latest keys and, for joiners, the member session.
struct Party {
    node: CallNode,
    keys: EpochKeys,
    member: Option<MemberHandle>,
}

struct Call {
    host: HostHandle,
    events: UnboundedReceiver<HostEvent>,
    addr: SocketAddr,
    relay: Option<NodeAddr>,
    parties: Vec<Party>,
}

async fn next_host_keys(events: &mut UnboundedReceiver<HostEvent>) -> EpochKeys {
    loop {
        match within(events.recv()).await.unwrap() {
            HostEvent::Keys(keys) => return keys,
            HostEvent::Error(e) => panic!("host error: {e}"),
            _ => {}
        }
    }
}

async fn next_member_keys(member: &mut MemberHandle) -> EpochKeys {
    match within(member.events.recv()).await.unwrap() {
        MemberEvent::Keys(keys) => keys,
        other => panic!("expected keys, got {other:?}"),
    }
}

async fn next_member_routes(member: &mut MemberHandle) -> Routes {
    match within(member.events.recv()).await.unwrap() {
        MemberEvent::Routes(routes) => routes,
        other => panic!("expected routes, got {other:?}"),
    }
}

impl Call {
    async fn start(relay: Option<NodeAddr>) -> Self {
        let identity = Identity::generate();
        let node = Node::bind(&identity, localhost()).unwrap();
        let addr = node.local_addr().unwrap();
        let (host, mut events) = HostHandle::start(&identity, ROOM).unwrap();
        let node = CallNode::start(&identity, node, ROOM, relay, Some(host.clone())).await;
        assert_eq!(node.has_rendezvous(), relay.is_some());
        let keys = next_host_keys(&mut events).await;
        let parties = vec![Party { node, keys, member: None }];
        Self { host, events, addr, relay, parties }
    }

    async fn invite(&self, addrs: Vec<String>) -> Invite {
        let relay = self.relay.map(|r| r.to_string());
        let link = self.host.invite(addrs, relay, now() + 600).await.unwrap();
        Invite::from_link(&link, now()).unwrap()
    }

    /// Admits a joiner and brings every earlier party to the new epoch.
    async fn admit(&mut self, invite: Invite, relay_only: bool) -> Path {
        let identity = Identity::generate();
        let joining = tokio::spawn(async move {
            let node = Node::bind(&identity, localhost()).unwrap();
            let mut node = CallNode::for_invite(&identity, node, &invite).await;
            node.set_relay_only(relay_only);
            let member = join(&node, &invite).await;
            (node, member)
        });
        loop {
            match within(self.events.recv()).await.unwrap() {
                HostEvent::Knock(device) => {
                    self.host.decide(device, true).await.unwrap();
                    break;
                }
                HostEvent::Error(e) => panic!("host error: {e}"),
                _ => {}
            }
        }
        let (node, member) = within(joining).await.unwrap();
        let mut member = member.unwrap();
        let path = member.path();
        let keys = next_member_keys(&mut member).await;
        self.parties[0].keys = next_host_keys(&mut self.events).await;
        for party in &mut self.parties[1..] {
            party.keys = next_member_keys(party.member.as_mut().unwrap()).await;
        }
        assert!(self.parties.iter().all(|p| p.keys.epoch == keys.epoch));
        self.parties.push(Party { node, keys, member: Some(member) });
        path
    }

    async fn admit_direct(&mut self) {
        let invite = self.invite(vec![self.addr.to_string()]).await;
        assert_eq!(self.admit(invite, false).await, Path::Direct);
    }

    /// The planner's view of the call: every member a participant, plus a forwarder node.
    fn directory(&self, me: usize, forwarder: Option<NodeAddr>) -> Directory {
        let roster = &self.parties[0].keys.roster;
        let mine = self.parties[me].node.id();
        let leaf = roster.iter().find(|(_, d)| *d == mine).unwrap().0;
        Directory {
            me: NodeId(leaf),
            members: roster
                .iter()
                .map(|(leaf, device)| (NodeId(*leaf), Peer { device: *device, addrs: Vec::new() }))
                .collect(),
            forwarders: forwarder.map(|f| (FORWARDER_ID, f)).into_iter().collect(),
        }
    }

    fn plan(&self, forwarded: bool) -> veil_topology::Plan {
        let mut nodes: Vec<veil_topology::Node> = self.parties[0]
            .keys
            .roster
            .iter()
            .map(|(leaf, _)| veil_topology::Node {
                id: NodeId(*leaf),
                kind: NodeKind::Peer,
                uplink_kbps: 100_000,
                downlink_kbps: 100_000,
                participant: true,
                can_forward: false,
            })
            .collect();
        nodes.push(veil_topology::Node {
            id: FORWARDER_ID,
            kind: NodeKind::SelfHosted,
            uplink_kbps: 1_000_000,
            downlink_kbps: 1_000_000,
            participant: false,
            can_forward: true,
        });
        let policy = Policy { mesh_max: if forwarded { 1 } else { 4 }, ..Policy::default() };
        plan(&nodes, &[], &policy).unwrap()
    }

    /// Starts every party's media session and applies the host's plan.
    async fn media(&mut self, forwarded: bool, forwarder: Option<NodeAddr>) -> Vec<MediaSession> {
        let mut sessions: Vec<MediaSession> =
            self.parties.iter_mut().map(|p| p.node.media().unwrap()).collect();
        self.replan(&mut sessions, forwarded, forwarder).await;
        sessions
    }

    /// The host publishes this epoch's plan; every party applies the routes it gets, all at
    /// once as in a real call (a member accepts a peer's dial only once its own routes
    /// expect that peer).
    async fn replan(
        &mut self,
        sessions: &mut Vec<MediaSession>,
        forwarded: bool,
        forwarder: Option<NodeAddr>,
    ) {
        let plan = self.plan(forwarded);
        let mine = self.host.publish_plan(plan, self.directory(0, forwarder)).await.unwrap();
        let mut all = vec![mine];
        for party in &mut self.parties[1..] {
            all.push(next_member_routes(party.member.as_mut().unwrap()).await);
        }
        let mut applying = Vec::new();
        for (mut media, routed) in sessions.drain(..).zip(all) {
            assert_eq!(matches!(routed, Routes::Forwarder { .. }), forwarded);
            applying.push(tokio::spawn(async move {
                let failed = within(media.apply(routed)).await;
                (media, failed)
            }));
        }
        for task in applying {
            let (media, failed) = task.await.unwrap();
            assert!(failed.is_empty(), "dial failures: {failed:?}");
            sessions.push(media);
        }
        for media in sessions.iter() {
            within(media.ready(Duration::from_secs(10))).await.unwrap();
        }
    }
}

/// Sends one sealed video frame from `from` and checks every other party opens it.
async fn frame_reaches_everyone(call: &mut Call, sessions: &mut [MediaSession], from: usize) {
    let body = format!("frame from party {from}").into_bytes();
    let sealed = call.parties[from].keys.sender.encrypt(&body).unwrap();
    let links = sessions[from].send_frame(7, &sealed).await;
    assert!(links >= 1);
    let leaf = call.directory(from, None).me.0;
    for (i, media) in sessions.iter_mut().enumerate() {
        if i == from {
            continue;
        }
        let packet = loop {
            let incoming = within(media.recv()).await.unwrap();
            if let Media::Video { frame_id, sealed } = incoming.media {
                assert_eq!(frame_id, 7);
                break sealed;
            }
        };
        let (sender, plain) = call.parties[i].keys.receiver.decrypt(&packet).unwrap();
        assert_eq!((sender, plain), (leaf, body.clone()), "party {i}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_call_punches_links_and_every_member_opens_each_frame() {
    let mut call = Call::start(Some(start_rendezvous())).await;
    call.admit_direct().await;
    call.admit_direct().await;
    let mut sessions = call.media(false, None).await;
    for media in &sessions {
        let paths = media.paths();
        assert_eq!(paths.len(), 2, "a link to each other member");
        // No addresses were in the directory, so leaders reached peers by punching.
        assert!(paths.iter().all(|(_, p)| matches!(p, Path::Punched | Path::Direct)));
    }
    for from in 0..3 {
        frame_reaches_everyone(&mut call, &mut sessions, from).await;
    }

    // Audio datagrams take the same links.
    let sealed = call.parties[1].keys.sender.encrypt(b"audio").unwrap();
    assert_eq!(sessions[1].send_audio(&sealed), 2);
    let incoming = loop {
        let incoming = within(sessions[2].recv()).await.unwrap();
        if let Media::Audio(packet) = incoming.media {
            break packet;
        }
    };
    assert_eq!(call.parties[2].keys.receiver.decrypt(&incoming).unwrap().1, b"audio");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forwarded_call_sends_each_frame_once_and_every_member_opens_it() {
    let forwarder = start_forwarder();
    let mut call = Call::start(None).await;
    call.admit_direct().await;
    call.admit_direct().await;
    let mut sessions = call.media(true, Some(forwarder)).await;
    for media in &sessions {
        assert_eq!(media.paths(), vec![(forwarder.key, Path::Forwarder)]);
    }
    // Let the forwarder register every member's readers before the first frame.
    tokio::time::sleep(Duration::from_millis(100)).await;
    for from in 0..3 {
        frame_reaches_everyone(&mut call, &mut sessions, from).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_follow_the_hosts_plan_through_a_join() {
    let mut call = Call::start(Some(start_rendezvous())).await;
    call.admit_direct().await;
    let mut sessions = call.media(false, None).await;
    frame_reaches_everyone(&mut call, &mut sessions, 1).await;

    let stale = call.directory(0, None);
    call.admit_direct().await;
    let refused = call.host.publish_plan(call.plan(false), stale).await;
    assert!(matches!(refused, Err(CallError::Plan(_))), "a last-epoch directory: {refused:?}");

    sessions.push(call.parties[2].node.media().unwrap());
    call.replan(&mut sessions, false, None).await;
    for media in &sessions {
        assert_eq!(media.paths().len(), 2, "a link to each other member");
    }
    for from in 0..3 {
        frame_reaches_everyone(&mut call, &mut sessions, from).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_forwarder_drops_a_member_the_host_removed() {
    let forwarder = start_forwarder();
    let mut call = Call::start(None).await;
    call.admit_direct().await;
    call.admit_direct().await;
    let mut sessions = call.media(true, Some(forwarder)).await;

    // Keep the removed member's node and media session alive: only the forwarder may end it.
    let removed = call.parties.remove(2);
    let removed_media = sessions.remove(2);
    call.host.remove(removed.node.id()).await.unwrap();
    call.parties[0].keys = next_host_keys(&mut call.events).await;
    call.parties[1].keys = next_member_keys(call.parties[1].member.as_mut().unwrap()).await;
    call.replan(&mut sessions, true, Some(forwarder)).await;

    within(async {
        while !removed_media.paths().is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    for from in 0..2 {
        frame_reaches_everyone(&mut call, &mut sessions, from).await;
    }
    drop((removed, removed_media));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_falls_back_when_the_direct_dial_fails() {
    let mut call = Call::start(Some(start_rendezvous())).await;
    // The invite's direct address is a node with another key: the pinned dial fails.
    let impostor = Node::bind(&Identity::generate(), localhost()).unwrap();
    let wrong = impostor.local_addr().unwrap().to_string();

    let invite = call.invite(vec![wrong.clone()]).await;
    let path = call.admit(invite, false).await;
    assert_ne!(path, Path::Direct, "the failed direct dial was not used");

    let invite = call.invite(vec![wrong]).await;
    assert_eq!(call.admit(invite, true).await, Path::Relayed);
    let link = call.parties[2].member.as_ref().unwrap().link();
    assert_eq!(link.remote_addr(), RELAY_PEER);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_only_members_exchange_media_through_the_relay() {
    let mut call = Call::start(Some(start_rendezvous())).await;
    call.parties[0].node.set_relay_only(true);
    let invite = call.invite(vec![call.addr.to_string()]).await;
    assert_eq!(call.admit(invite, true).await, Path::Relayed);
    let mut sessions = call.media(false, None).await;
    for media in &sessions {
        let paths = media.paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].1, Path::Relayed);
    }
    frame_reaches_everyone(&mut call, &mut sessions, 0).await;
    frame_reaches_everyone(&mut call, &mut sessions, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_sealed_before_an_epoch_change_opens_in_the_grace_period() {
    let mut call = Call::start(None).await;
    call.admit_direct().await;
    let mut ring = KeyRing::new(call.parties[0].keys.epoch, take_receiver(&mut call.parties[0].keys));
    let late = call.parties[1].keys.sender.encrypt(b"sealed in epoch 1").unwrap();
    let too_late = call.parties[1].keys.sender.encrypt(b"also epoch 1").unwrap();

    call.admit_direct().await;
    let changed = Instant::now();
    ring.rotate(call.parties[0].keys.epoch, take_receiver(&mut call.parties[0].keys), changed);
    assert_eq!(ring.epoch(), 2);
    assert_eq!(ring.open(&late, changed).unwrap().1, b"sealed in epoch 1");
    let fresh = call.parties[2].keys.sender.encrypt(b"epoch 2").unwrap();
    assert_eq!(ring.open(&fresh, changed).unwrap().1, b"epoch 2");
    let after = changed + veil_call::keyring::GRACE;
    assert_eq!(ring.open(&too_late, after), Err(SecurityError::UnknownKey));
}

/// Moves a party's receiver into a ring, leaving a receiver that opens nothing.
fn take_receiver(keys: &mut EpochKeys) -> veil_security::frame::FrameReceiver {
    let empty = veil_security::frame::FrameReceiver::new(&[0; 32], keys.epoch).unwrap();
    std::mem::replace(&mut keys.receiver, empty)
}

/// Waits for the next keyframe request, skipping media.
async fn next_keyframe_request(media: &mut MediaSession) {
    loop {
        if within(media.recv()).await.unwrap().media == Media::KeyframeRequest {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keyframe_requests_reach_the_sender_in_a_mesh_and_through_a_forwarder() {
    for forwarded in [false, true] {
        let forwarder = forwarded.then(start_forwarder);
        let rendezvous = (!forwarded).then(start_rendezvous);
        let mut call = Call::start(rendezvous).await;
        call.admit_direct().await;
        call.admit_direct().await;
        let mut sessions = call.media(forwarded, forwarder).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let sender = call.parties[0].node.id();
        assert!(within(sessions[2].request_keyframe(sender)).await.unwrap());
        next_keyframe_request(&mut sessions[0]).await;
        let quiet = tokio::time::timeout(Duration::from_millis(300), sessions[1].recv()).await;
        assert!(quiet.is_err(), "only the named sender is asked (forwarded: {forwarded})");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_messages_reach_every_member_in_a_mesh_and_through_a_forwarder() {
    for forwarded in [false, true] {
        let forwarder = forwarded.then(start_forwarder);
        let rendezvous = (!forwarded).then(start_rendezvous);
        let mut call = Call::start(rendezvous).await;
        call.admit_direct().await;
        call.admit_direct().await;
        let mut sessions = call.media(forwarded, forwarder).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let message = AppMessage::chat("hello from party 1").unwrap();
        let sealed = message.seal(&mut call.parties[1].keys.sender).unwrap();
        assert!(within(sessions[1].send_app(&sealed)).await >= 1);
        let leaf = call.directory(1, None).me.0;
        for i in [0, 2] {
            let packet = loop {
                if let Media::App(packet) = within(sessions[i].recv()).await.unwrap().media {
                    break packet;
                }
            };
            assert_eq!(packet, sealed, "passed on unchanged (forwarded: {forwarded})");
            let (sender, plain) = call.parties[i].keys.receiver.decrypt(&packet).unwrap();
            assert_eq!(sender, leaf);
            assert_eq!(AppMessage::decode(&plain).unwrap(), message);
        }
        let too_big = vec![0; veil_net::control::MAX_APP_LEN + 1];
        assert_eq!(sessions[1].send_app(&too_big).await, 0);
    }
}
