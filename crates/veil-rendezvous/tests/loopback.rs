//! Rendezvous, introduction and relay between nodes on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::Node;
use veil_net::control::Control;
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::socket::RELAY_PEER;
use veil_rendezvous::{Refusal, RendezvousClient, RendezvousError, RendezvousServer, punch, room_tag};
use veil_security::frame::{FrameReceiver, FrameSender};
use veil_security::{Identity, PublicId};

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Starts a rendezvous server and returns its address and key.
fn start_server() -> (SocketAddr, PublicId) {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node);
    let addr = server.local_addr().unwrap();
    let key = server.id();
    tokio::spawn(server.run());
    (addr, key)
}

struct Member {
    identity: Identity,
    node: Node,
    client: RendezvousClient,
}

async fn member(server: (SocketAddr, PublicId), room: [u8; 32]) -> Member {
    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let mut client = RendezvousClient::connect(&node, &[server.0], server.1).await.unwrap();
    let local = node.local_addr().unwrap();
    let observed = client.register(room, vec![local]).await.unwrap();
    assert_eq!(observed, local, "server reflects the address it sees");
    Member {
        identity,
        node,
        client,
    }
}

async fn with_timeout<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn introduction_lets_two_members_connect_directly() {
    let server = start_server();
    let room = room_tag(&[1; 16]);
    let mut a = member(server, room).await;
    let mut b = member(server, room).await;
    let b_key = b.identity.public();
    let a_key = a.identity.public();

    let plan_a = with_timeout(a.client.introduce(b_key)).await.unwrap();
    assert_eq!(plan_a.peer, b_key);
    let ServerMsg::Punch { peer, .. } = with_timeout(b.client.next_event()).await.unwrap() else {
        panic!("b should be told to punch");
    };
    assert_eq!(peer, a_key);

    // b would dial too; on localhost a's dial alone is enough. b's node accepts it.
    let accept = tokio::spawn(async move {
        loop {
            let link = b.node.accept().await.unwrap().unwrap();
            if link.peer() == a_key {
                return link.peer();
            }
        }
    });
    let link = with_timeout(punch(&a.node, &plan_a)).await.unwrap();
    assert_eq!(link.peer(), b_key);
    assert_eq!(with_timeout(accept).await.unwrap(), a_key);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_of_another_room_are_not_introduced() {
    let server = start_server();
    let mut a = member(server, room_tag(&[1; 16])).await;
    let stranger = member(server, room_tag(&[2; 16])).await;
    let result = with_timeout(a.client.introduce(stranger.identity.public())).await;
    assert!(matches!(
        result,
        Err(RendezvousError::Refused(Refusal::PeerNotInRoom))
    ));
    let relay = with_timeout(a.client.open_relay(stranger.identity.public())).await;
    assert!(matches!(
        relay,
        Err(RendezvousError::Refused(Refusal::PeerNotInRoom))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_carries_a_pinned_end_to_end_link() {
    let server = start_server();
    let room = room_tag(&[7; 16]);
    let mut a = member(server, room).await;
    let mut b = member(server, room).await;
    let a_key = a.identity.public();
    let b_key = b.identity.public();

    let session = with_timeout(a.client.open_relay(b_key)).await.unwrap();
    let ServerMsg::RelayReady { session: b_session, peer } =
        with_timeout(b.client.next_event()).await.unwrap()
    else {
        panic!("b should be told about the relay");
    };
    assert_eq!((b_session, peer), (session, a_key));

    // Each side binds a second node to its relay socket. Same identities, same pinning.
    let a_inner = Node::bind_virtual(&a.identity, a.client.relay_socket(session).unwrap()).unwrap();
    let b_inner = Node::bind_virtual(&b.identity, b.client.relay_socket(session).unwrap()).unwrap();

    let epoch_secret = [9u8; 32];
    let frame: Vec<u8> = (0..300_000u32).map(|i| (i % 249) as u8).collect();
    let expected = frame.clone();

    let callee = tokio::spawn(async move {
        let link = b_inner.accept().await.unwrap().unwrap();
        assert_eq!(link.peer(), a_key);
        let mut ctl = link.accept_control().await.unwrap();
        assert_eq!(
            ctl.recv().await.unwrap(),
            Control::Hello { room_id: [7; 16], join_token: [8; 16] }
        );
        ctl.send(&Control::Admitted).await.unwrap();
        let (_, sealed) = link.recv_frame().await.unwrap();
        let mut keys = FrameReceiver::new(&epoch_secret, 0).unwrap();
        keys.decrypt(&sealed).unwrap().1
    });

    let link = with_timeout(a_inner.connect(&[RELAY_PEER], b_key)).await.unwrap();
    assert_eq!(link.peer(), b_key);
    let mut ctl = link.open_control().await.unwrap();
    ctl.send(&Control::Hello { room_id: [7; 16], join_token: [8; 16] })
        .await
        .unwrap();
    assert_eq!(with_timeout(ctl.recv()).await.unwrap(), Control::Admitted);
    let sealed = FrameSender::new(&epoch_secret, 0, 1).unwrap().encrypt(&frame).unwrap();
    with_timeout(link.send_frame(0, &sealed)).await.unwrap();

    let got = with_timeout(callee).await.unwrap();
    assert_eq!(got, expected);
    link.close("done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relayed_dial_to_the_wrong_key_fails() {
    let server = start_server();
    let room = room_tag(&[5; 16]);
    let mut a = member(server, room).await;
    let mut b = member(server, room).await;
    let session = with_timeout(a.client.open_relay(b.identity.public())).await.unwrap();
    let _ = with_timeout(b.client.next_event()).await;

    let a_inner = Node::bind_virtual(&a.identity, a.client.relay_socket(session).unwrap()).unwrap();
    let b_inner = Node::bind_virtual(&b.identity, b.client.relay_socket(session).unwrap()).unwrap();
    tokio::spawn(async move { while b_inner.accept().await.is_some() {} });

    // The relay delivers to b, but a expected someone else: the handshake must fail.
    let someone_else = Identity::generate().public();
    let result = with_timeout(a_inner.connect(&[RELAY_PEER], someone_else)).await;
    assert!(result.is_err());
}
