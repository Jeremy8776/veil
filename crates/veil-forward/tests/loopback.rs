//! Forwarding between members on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_forward::{ForwardError, ForwarderClient, ForwarderServer, Limits, Refusal, RosterTicket, Wanted};
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
