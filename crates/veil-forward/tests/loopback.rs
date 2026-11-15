//! Forwarding between members on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_forward::{ForwardError, ForwarderClient, ForwarderServer, Limits, Refusal};
use veil_net::Node;
use veil_rendezvous::room_tag;
use veil_security::frame::{FrameReceiver, FrameSender};
use veil_security::{Identity, PublicId};

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

struct Member {
    _node: Node,
    client: ForwarderClient,
}

async fn join(forwarder: (SocketAddr, PublicId), room: [u8; 32]) -> Result<Member, ForwardError> {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let client = ForwarderClient::join(&node, &[forwarder.0], forwarder.1, room).await?;
    Ok(Member { _node: node, client })
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
    let room = room_tag(&[1; 16]);
    let a = join(forwarder, room).await.unwrap();
    let b = join(forwarder, room).await.unwrap();
    let c = join(forwarder, room).await.unwrap();
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
    let room = room_tag(&[2; 16]);
    let a = join(forwarder, room).await.unwrap();
    let b = join(forwarder, room).await.unwrap();
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
    let a = join(forwarder, room_tag(&[3; 16])).await.unwrap();
    let other = join(forwarder, room_tag(&[4; 16])).await.unwrap();
    settle().await;
    a.client.link().send_frame(1, &[0u8; 64]).await.unwrap();
    assert!(nothing_arrives(other.client.link().recv_frame()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_leaving_does_not_break_the_room() {
    let forwarder = start(Limits::default());
    let room = room_tag(&[5; 16]);
    let a = join(forwarder, room).await.unwrap();
    let b = join(forwarder, room).await.unwrap();
    let c = join(forwarder, room).await.unwrap();
    drop(b);
    settle().await;
    a.client.link().send_frame(9, &[1u8; 64]).await.unwrap();
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
    let room = room_tag(&[6; 16]);
    let _a = join(forwarder, room).await.unwrap();
    let _b = join(forwarder, room).await.unwrap();
    assert!(matches!(
        join(forwarder, room).await,
        Err(ForwardError::Refused(Refusal::RoomFull))
    ));
    assert!(matches!(
        join(forwarder, room_tag(&[7; 16])).await,
        Err(ForwardError::Refused(Refusal::Full))
    ));
}
