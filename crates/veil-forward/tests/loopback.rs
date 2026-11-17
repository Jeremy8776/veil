//! Forwarding between members on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use veil_forward::protocol::ServerMsg;
use veil_forward::{ForwardError, ForwarderClient, ForwarderServer, Limits, QueueMonitor, Refusal, RosterTicket, Wanted};
use veil_net::Node;
use veil_rendezvous::room_tag;
use veil_security::frame::{FrameReceiver, FrameSender};
use veil_security::{Identity, Layer, PublicId};

const SECRET: [u8; 32] = [3; 32];

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn start(limits: Limits) -> (SocketAddr, PublicId) {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = ForwarderServer::new(node, limits);
    let addr = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());
    addr
}

/// A room as its host sees it: the host signs a ticket per epoch.
struct Room {
    host: Identity,
    tag: [u8; 32],
}

impl Room {
    fn new(id: u8) -> Self {
        Self { host: Identity::generate(), tag: room_tag(&[id; 16]) }
    }

    fn ticket(&self, epoch: u64, members: &[&Identity]) -> Vec<u8> {
        RosterTicket {
            room_tag: self.tag,
            host: self.host.public(),
            epoch,
            members: members.iter().map(|m| m.public()).collect(),
        }
        .seal(&self.host)
        .unwrap()
    }
}

struct Member {
    _node: Node,
    client: ForwarderClient,
}

async fn join_as(
    forwarder: (SocketAddr, PublicId),
    identity: &Identity,
    ticket: &[u8],
) -> Result<Member, ForwardError> {
    let node = Node::bind(identity, localhost()).unwrap();
    let client = ForwarderClient::join(&node, &[forwarder.0], forwarder.1, ticket).await?;
    Ok(Member { _node: node, client })
}

/// Joins a fresh device under an epoch 0 ticket that lists it.
async fn join(forwarder: (SocketAddr, PublicId), room: &Room) -> Result<Member, ForwardError> {
    let identity = Identity::generate();
    join_as(forwarder, &identity, &room.ticket(0, &[&identity])).await
}

async fn with_timeout<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f).await.expect("timed out")
}

async fn nothing_arrives<T>(f: impl std::future::Future<Output = T>) -> bool {
    tokio::time::timeout(Duration::from_millis(400), f).await.is_err()
}

/// Waits until the forwarder has registered everyone: joins are answered after registration,
/// so this only gives the server's reader tasks a moment to start.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sealed_frames_reach_every_other_member_unchanged() {
    let forwarder = start(Limits::default());
    let room = Room::new(1);
    let a = join(forwarder, &room).await.unwrap();
    let b = join(forwarder, &room).await.unwrap();
    let c = join(forwarder, &room).await.unwrap();
    assert_eq!(c.client.members_at_join(), 3);
    settle().await;

    let mut tx = FrameSender::new(&SECRET, 1, 0).unwrap();
    let sealed = tx.encrypt(&vec![7u8; 200_000]).unwrap();
    a.client.link().send_frame(42, &sealed).await.unwrap();

    for receiver in [&b, &c] {
        let (id, got) = with_timeout(receiver.client.link().recv_frame()).await.unwrap();
        assert_eq!((id, &got), (42, &sealed), "forwarded byte for byte");
        let mut rx = FrameReceiver::new(&SECRET, 1).unwrap();
        assert_eq!(rx.decrypt(&got).unwrap().0, 0, "receiver still sees the true sender");
    }
    assert!(nothing_arrives(a.client.link().recv_frame()).await, "never echoed to the sender");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audio_datagrams_are_forwarded() {
    let forwarder = start(Limits::default());
    let room = Room::new(2);
    let a = join(forwarder, &room).await.unwrap();
    let b = join(forwarder, &room).await.unwrap();
    settle().await;
    // Datagrams can be lost even on localhost, so send a few.
    for _ in 0..5 {
        a.client.link().send_datagram(b"opus packet").unwrap();
    }
    let got = with_timeout(b.client.link().recv_datagram()).await.unwrap();
    assert_eq!(&got[..], b"opus packet");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rooms_are_isolated() {
    let forwarder = start(Limits::default());
    let a = join(forwarder, &Room::new(3)).await.unwrap();
    let other = join(forwarder, &Room::new(4)).await.unwrap();
    settle().await;
    a.client.link().send_frame(1, &[0u8; 64]).await.unwrap();
    assert!(nothing_arrives(other.client.link().recv_frame()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_leaving_does_not_break_the_room() {
    let forwarder = start(Limits::default());
    let room = Room::new(5);
    let a = join(forwarder, &room).await.unwrap();
    let b = join(forwarder, &room).await.unwrap();
    let c = join(forwarder, &room).await.unwrap();
    drop(b);
    settle().await;
    a.client.link().send_frame(9, &[0u8; 64]).await.unwrap();
    let (id, _) = with_timeout(c.client.link().recv_frame()).await.unwrap();
    assert_eq!(id, 9);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limits_are_enforced() {
    let forwarder = start(Limits {
        max_rooms: 1,
        max_members_per_room: 2,
        ..Limits::default()
    });
    let room = Room::new(6);
    let _a = join(forwarder, &room).await.unwrap();
    let _b = join(forwarder, &room).await.unwrap();
    assert!(matches!(
        join(forwarder, &room).await,
        Err(ForwardError::Refused(Refusal::RoomFull))
    ));
    assert!(matches!(
        join(forwarder, &Room::new(7)).await,
        Err(ForwardError::Refused(Refusal::Full))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_devices_on_a_valid_ticket_may_join() {
    let forwarder = start(Limits::default());
    let room = Room::new(8);
    let listed = Identity::generate();
    let stranger = Identity::generate();
    let ticket = room.ticket(0, &[&listed]);
    assert!(matches!(
        join_as(forwarder, &stranger, &ticket).await,
        Err(ForwardError::Refused(Refusal::NotListed))
    ));
    let mut forged = ticket.clone();
    forged[3] ^= 1;
    assert!(matches!(
        join_as(forwarder, &listed, &forged).await,
        Err(ForwardError::Refused(Refusal::BadTicket))
    ));
    assert!(join_as(forwarder, &listed, &ticket).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forwarder_for_one_host_refuses_other_hosts() {
    let mine = Room::new(9);
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = ForwarderServer::new(node, Limits::default()).only_host(mine.host.public());
    let forwarder = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());

    let stranger_room = Room::new(9);
    let stranger = Identity::generate();
    assert!(matches!(
        join_as(forwarder, &stranger, &stranger_room.ticket(0, &[&stranger])).await,
        Err(ForwardError::Refused(Refusal::NotServed))
    ));
    let member = Identity::generate();
    assert!(join_as(forwarder, &member, &mine.ticket(0, &[&member])).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ticket_from_another_host_opens_a_separate_room() {
    let forwarder = start(Limits::default());
    let room = Room::new(9);
    let a = join(forwarder, &room).await.unwrap();
    // A squatter who knows the tag signs its own ticket: it lands in a room of its own.
    let squat = Room { host: Identity::generate(), tag: room.tag };
    let squatter = join(forwarder, &squat).await.unwrap();
    assert_eq!(squatter.client.members_at_join(), 1);
    let b = join(forwarder, &room).await.unwrap();
    assert_eq!(b.client.members_at_join(), 2);
    settle().await;
    a.client.link().send_frame(1, &[0u8; 64]).await.unwrap();
    with_timeout(b.client.link().recv_frame()).await.unwrap();
    assert!(nothing_arrives(squatter.client.link().recv_frame()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newer_ticket_drops_removed_members_and_old_tickets_stop_working() {
    let forwarder = start(Limits::default());
    let room = Room::new(10);
    let (ia, ib, ic) = (Identity::generate(), Identity::generate(), Identity::generate());
    let first = room.ticket(1, &[&ia, &ib, &ic]);
    let a = join_as(forwarder, &ia, &first).await.unwrap();
    let b = join_as(forwarder, &ib, &first).await.unwrap();
    let c = join_as(forwarder, &ic, &first).await.unwrap();

    // The host removed b; a passes on the next epoch's ticket.
    a.client.update_ticket(&room.ticket(2, &[&ia, &ic])).await.unwrap();
    with_timeout(b.client.link().quic().closed()).await;
    drop(b);
    assert!(matches!(
        join_as(forwarder, &ib, &first).await,
        Err(ForwardError::Refused(Refusal::StaleTicket))
    ));

    settle().await;
    a.client.link().send_frame(5, &[0u8; 64]).await.unwrap();
    let (id, _) = with_timeout(c.client.link().recv_frame()).await.unwrap();
    assert_eq!(id, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receiver_gets_only_the_layers_it_subscribes_to() {
    let forwarder = start(Limits::default());
    let room = Room::new(40);
    let (alice, bob, carol) = (Identity::generate(), Identity::generate(), Identity::generate());
    let ticket = room.ticket(0, &[&alice, &bob, &carol]);
    let a = join_as(forwarder, &alice, &ticket).await.unwrap();
    let b = join_as(forwarder, &bob, &ticket).await.unwrap();
    let c = join_as(forwarder, &carol, &ticket).await.unwrap();
    b.client.subscribe(&[(alice.public(), Wanted::THUMB)]).await.unwrap();
    settle().await;

    let mut tx = FrameSender::new(&SECRET, 0, 0).unwrap();
    let base = tx.encrypt_layer(Layer::new(0, 0, true).unwrap(), b"thumb").unwrap();
    let uhd = tx.encrypt_layer(Layer::new(2, 0, true).unwrap(), b"uhd").unwrap();
    a.client.link().send_frame(1, &base).await.unwrap();
    a.client.link().send_frame(2, &uhd).await.unwrap();

    let (id, _) = with_timeout(b.client.link().recv_frame()).await.unwrap();
    assert_eq!(id, 1, "thumbnail subscriber gets the base layer");
    assert!(nothing_arrives(b.client.link().recv_frame()).await, "and not the uhd layer");
    let mut got = vec![
        with_timeout(c.client.link().recv_frame()).await.unwrap().0,
        with_timeout(c.client.link().recv_frame()).await.unwrap().0,
    ];
    got.sort_unstable();
    assert_eq!(got, [1, 2], "a receiver that never subscribed gets every layer");

    // Raising waits for a keyframe: a mid-group uhd frame is still dropped.
    b.client.subscribe(&[(alice.public(), Wanted::ALL)]).await.unwrap();
    settle().await;
    let mid = tx.encrypt_layer(Layer::new(2, 0, false).unwrap(), b"mid").unwrap();
    a.client.link().send_frame(3, &mid).await.unwrap();
    assert!(nothing_arrives(b.client.link().recv_frame()).await);
    let key = tx.encrypt_layer(Layer::new(0, 0, true).unwrap(), b"key").unwrap();
    a.client.link().send_frame(4, &key).await.unwrap();
    assert_eq!(with_timeout(b.client.link().recv_frame()).await.unwrap().0, 4);
    let next = tx.encrypt_layer(Layer::new(2, 0, false).unwrap(), b"next").unwrap();
    a.client.link().send_frame(5, &next).await.unwrap();
    assert_eq!(with_timeout(b.client.link().recv_frame()).await.unwrap().0, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keyframe_requests_reach_only_the_named_sender() {
    let forwarder = start(Limits::default());
    let room = Room::new(41);
    let (alice, bob, carol) = (Identity::generate(), Identity::generate(), Identity::generate());
    let ticket = room.ticket(0, &[&alice, &bob, &carol]);
    let a = join_as(forwarder, &alice, &ticket).await.unwrap();
    let b = join_as(forwarder, &bob, &ticket).await.unwrap();
    let c = join_as(forwarder, &carol, &ticket).await.unwrap();
    settle().await;

    b.client.request_keyframe(alice.public()).await.unwrap();
    assert_eq!(with_timeout(a.client.recv()).await.unwrap(), ServerMsg::KeyframeRequest);
    assert!(nothing_arrives(c.client.recv()).await, "carol was not named");

    // A request naming a device outside the room goes nowhere.
    b.client.request_keyframe(Identity::generate().public()).await.unwrap();
    assert!(nothing_arrives(a.client.recv()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscription_raise_asks_the_sender_for_a_keyframe() {
    let forwarder = start(Limits::default());
    let room = Room::new(42);
    let (alice, bob) = (Identity::generate(), Identity::generate());
    let ticket = room.ticket(0, &[&alice, &bob]);
    let a = join_as(forwarder, &alice, &ticket).await.unwrap();
    let b = join_as(forwarder, &bob, &ticket).await.unwrap();
    settle().await;

    b.client.subscribe(&[(alice.public(), Wanted::THUMB)]).await.unwrap();
    assert!(nothing_arrives(a.client.recv()).await, "lowering needs no keyframe");
    b.client.subscribe(&[(alice.public(), Wanted::ALL)]).await.unwrap();
    assert_eq!(with_timeout(a.client.recv()).await.unwrap(), ServerMsg::KeyframeRequest);
}

const MIB: usize = 1024 * 1024;

/// A forwarder whose queued total the test can read.
fn start_monitored(limits: Limits) -> ((SocketAddr, PublicId), QueueMonitor) {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = ForwarderServer::new(node, limits);
    let addr = (server.local_addr().unwrap(), server.id());
    let monitor = server.monitor();
    tokio::spawn(server.run());
    (addr, monitor)
}

/// Eight two-member rooms; every member sends `frames` frames of `len` bytes and reads nothing.
/// Returns the members, still connected.
async fn fill_unread_rooms(forwarder: (SocketAddr, PublicId), frames: u32, len: usize) -> Vec<Member> {
    let mut members = Vec::new();
    for n in 0..8u8 {
        let room = Room::new(100 + n);
        let (alice, bob) = (Identity::generate(), Identity::generate());
        let ticket = room.ticket(0, &[&alice, &bob]);
        members.push(join_as(forwarder, &alice, &ticket).await.unwrap());
        members.push(join_as(forwarder, &bob, &ticket).await.unwrap());
    }
    settle().await;
    let mut tx = FrameSender::new(&SECRET, 1, 0).unwrap();
    for id in 0..frames {
        let sealed = tx.encrypt(&vec![9u8; len]).unwrap();
        for member in &members {
            // The forwarder reads promptly, so these sends finish although nobody reads back.
            with_timeout(member.client.link().send_frame(id, &sealed)).await.unwrap();
        }
    }
    members
}

/// The highest queued total seen over `window`.
async fn peak_queued(monitor: &QueueMonitor, window: Duration) -> usize {
    let start = Instant::now();
    let mut peak = 0;
    while start.elapsed() < window {
        peak = peak.max(monitor.queued_bytes());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    peak
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unread_receivers_cannot_pin_more_than_the_node_budget_and_frames_expire() {
    let node_budget = 4 * MIB;
    let (forwarder, monitor) = start_monitored(Limits {
        node_queue_bytes: node_budget,
        send_deadline: Duration::from_millis(300),
        ..Limits::default()
    });
    // Larger than the stream window (1.25 MB), so the first send to an unread receiver blocks.
    let members = fill_unread_rooms(forwarder, 3, 2 * MIB).await;
    let peak = peak_queued(&monitor, Duration::from_millis(100)).await;
    assert!(peak > 0, "frames were queued behind receivers that do not read");
    assert!(peak <= node_budget, "queued {peak} exceeds the node budget {node_budget}");

    // Everything expires: stuck sends are abandoned and stale frames dropped.
    let start = Instant::now();
    while monitor.queued_bytes() != 0 {
        assert!(start.elapsed() < Duration::from_millis(1500), "queue did not drain: {}", monitor.queued_bytes());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A receiver that starts reading after the deadline gets none of the old frames.
    for member in &members {
        loop {
            match tokio::time::timeout(Duration::from_millis(300), member.client.link().recv_frame()).await {
                Err(_) => break,
                Ok(Ok((id, _))) => panic!("old frame {id} arrived after its deadline"),
                Ok(Err(_)) => {}
            }
        }
    }
    assert_eq!(monitor.queued_bytes(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_source_block_is_held_to_its_share_below_the_node_budget() {
    let source_budget = 4 * MIB;
    let (forwarder, monitor) = start_monitored(Limits {
        node_queue_bytes: 64 * MIB,
        source_queue_bytes: source_budget,
        send_deadline: Duration::from_secs(5),
        ..Limits::default()
    });
    // Every link is from 127.0.0.1: one block.
    let members = fill_unread_rooms(forwarder, 3, 2 * MIB).await;
    let peak = peak_queued(&monitor, Duration::from_millis(300)).await;
    assert!(peak >= 2 * MIB, "the share was used: {peak}");
    assert!(peak <= source_budget, "queued {peak} exceeds the source share {source_budget}");
    drop(members);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixteen_reading_members_exchange_large_frames_under_default_limits() {
    let (forwarder, monitor) = start_monitored(Limits::default());
    let room = Room::new(60);
    let identities: Vec<Identity> = (0..16).map(|_| Identity::generate()).collect();
    let refs: Vec<&Identity> = identities.iter().collect();
    let ticket = room.ticket(0, &refs);
    let mut members = Vec::new();
    for identity in &identities {
        members.push(std::sync::Arc::new(join_as(forwarder, identity, &ticket).await.unwrap()));
    }
    settle().await;

    let mut tx = FrameSender::new(&SECRET, 1, 0).unwrap();
    for (n, sender) in (0u32..).zip(&members) {
        let sealed = tx.encrypt(&vec![9u8; MIB]).unwrap();
        let mut readers = tokio::task::JoinSet::new();
        for (m, receiver) in (0u32..).zip(&members) {
            if m != n {
                let receiver = receiver.clone();
                readers.spawn(async move { receiver.client.link().recv_frame().await });
            }
        }
        sender.client.link().send_frame(n, &sealed).await.unwrap();
        let mut got = 0;
        while let Some(done) = with_timeout(readers.join_next()).await {
            let (id, frame) = done.unwrap().unwrap();
            assert_eq!((id, &frame), (n, &sealed));
            got += 1;
        }
        assert_eq!(got, 15, "every other member received frame {n}");
    }
    let start = Instant::now();
    while monitor.queued_bytes() != 0 {
        assert!(start.elapsed() < Duration::from_secs(3), "bytes were not released");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
