//! Trust-boundary regressions using isolated loopback endpoints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::{FramedRecv, FramedSend, Link, Node};
use veil_rendezvous::RendezvousServer;
use veil_rendezvous::protocol::{Ask, ClientMsg, ServerMsg};
use veil_security::{Identity, PublicId};

struct Peer {
    _node: Node,
    _link: Link,
    out: FramedSend,
    input: FramedRecv,
}

async fn register(
    server: (SocketAddr, PublicId),
    identity: &Identity,
    room: [u8; 32],
    host: PublicId,
) -> Peer {
    register_with_hints(server, identity, room, host, Vec::new()).await
}

async fn register_with_hints(
    server: (SocketAddr, PublicId),
    identity: &Identity,
    room: [u8; 32],
    host: PublicId,
    local_addrs: Vec<SocketAddr>,
) -> Peer {
    let node = Node::bind(identity, "127.0.0.1:0".parse().unwrap()).unwrap();
    let link = node.connect(&[server.0], server.1).await.unwrap();
    let (mut out, mut input) = link.open_control().await.unwrap().split();
    out.send(&ClientMsg::Register {
        room_tag: room,
        host,
        pass: None,
        local_addrs,
    })
    .await
    .unwrap();
    assert!(matches!(
        input.recv::<ServerMsg>().await.unwrap(),
        ServerMsg::Registered { .. }
    ));
    Peer {
        _node: node,
        _link: link,
        out,
        input,
    }
}

async fn allow(host: &mut Peer, member: PublicId) {
    host.out
        .send(&ClientMsg::Allow {
            members: vec![member],
            passes: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        host.input.recv::<ServerMsg>().await.unwrap(),
        ServerMsg::Allowed
    );
}

async fn room_scoped_consent(kind: Ask) {
    let server_node = Node::bind(&Identity::generate(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let server = RendezvousServer::new(server_node);
    let address = (server.local_addr().unwrap(), server.id());
    let task = tokio::spawn(server.run());
    let host = Identity::generate();
    let member = Identity::generate();
    let mut first_host = register(address, &host, [1; 32], host.public()).await;
    let mut second_host = register(address, &host, [2; 32], host.public()).await;
    allow(&mut first_host, member.public()).await;
    allow(&mut second_host, member.public()).await;
    let mut first_member = register(address, &member, [1; 32], host.public()).await;
    let mut second_member = register(address, &member, [2; 32], host.public()).await;

    let request = match kind {
        Ask::Introduce => ClientMsg::Introduce {
            peer: host.public(),
        },
        Ask::Relay => ClientMsg::OpenRelay {
            peer: host.public(),
        },
    };
    first_member.out.send(&request).await.unwrap();
    assert_eq!(
        first_host.input.recv::<ServerMsg>().await.unwrap(),
        ServerMsg::Request {
            from: member.public(),
            kind
        }
    );
    // The same identities in another room cannot spend this room's pending consent.
    second_host
        .out
        .send(&ClientMsg::Accept {
            from: member.public(),
            kind,
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            second_host.input.recv::<ServerMsg>()
        )
        .await
        .is_err()
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            second_member.input.recv::<ServerMsg>()
        )
        .await
        .is_err()
    );

    // Nor may the wrong-room acceptance consume the genuine pending request.
    first_host
        .out
        .send(&ClientMsg::Accept {
            from: member.public(),
            kind,
        })
        .await
        .unwrap();
    let received = tokio::time::timeout(
        Duration::from_secs(2),
        first_member.input.recv::<ServerMsg>(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        (kind, received),
        (Ask::Introduce, ServerMsg::Punch { .. }) | (Ask::Relay, ServerMsg::RelayReady { .. })
    ));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn introduction_consent_is_scoped_to_the_registered_room() {
    room_scoped_consent(Ask::Introduce).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_consent_is_scoped_to_the_registered_room() {
    room_scoped_consent(Ask::Relay).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn introductions_use_the_authenticated_endpoint_instead_of_claimed_addresses() {
    let server_node = Node::bind(&Identity::generate(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let server = RendezvousServer::new(server_node);
    let address = (server.local_addr().unwrap(), server.id());
    let task = tokio::spawn(server.run());
    let host = Identity::generate();
    let member = Identity::generate();
    let mut hosting = register_with_hints(
        address,
        &host,
        [3; 32],
        host.public(),
        vec![
            "127.0.0.99:9000".parse().unwrap(),
            "127.0.0.1:9".parse().unwrap(),
        ],
    )
    .await;
    allow(&mut hosting, member.public()).await;
    let mut joining = register(address, &member, [3; 32], host.public()).await;
    joining
        .out
        .send(&ClientMsg::Introduce {
            peer: host.public(),
        })
        .await
        .unwrap();
    assert_eq!(
        hosting.input.recv::<ServerMsg>().await.unwrap(),
        ServerMsg::Request {
            from: member.public(),
            kind: Ask::Introduce
        }
    );
    hosting
        .out
        .send(&ClientMsg::Accept {
            from: member.public(),
            kind: Ask::Introduce,
        })
        .await
        .unwrap();
    let ServerMsg::Punch { addrs, .. } = joining.input.recv::<ServerMsg>().await.unwrap() else {
        panic!("expected an introduction");
    };
    assert_eq!(addrs, vec![hosting._node.local_addr().unwrap()]);
    task.abort();
}

async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future).await.expect("bounded operation")
}

struct TwoRooms {
    first_host: Peer,
    first_member: Peer,
    second_host: Peer,
    second_member: Peer,
    host: Identity,
    member: Identity,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TwoRooms {
    fn drop(&mut self) { self.task.abort(); }
}

impl TwoRooms {
    async fn start() -> Self {
        let node = Node::bind(&Identity::generate(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let server = RendezvousServer::new(node);
        let address = (server.local_addr().unwrap(), server.id());
        let task = tokio::spawn(server.run());
        let host = Identity::generate();
        let member = Identity::generate();
        let mut first_host = register(address, &host, [71; 32], host.public()).await;
        let mut second_host = register(address, &host, [72; 32], host.public()).await;
        allow(&mut first_host, member.public()).await;
        allow(&mut second_host, member.public()).await;
        let first_member = register(address, &member, [71; 32], host.public()).await;
        let second_member = register(address, &member, [72; 32], host.public()).await;
        Self { first_host, first_member, second_host, second_member, host, member, task }
    }

    async fn request_second(&mut self, kind: Ask) {
        let request = match kind {
            Ask::Introduce => ClientMsg::Introduce { peer: self.host.public() },
            Ask::Relay => ClientMsg::OpenRelay { peer: self.host.public() },
        };
        self.second_member.out.send(&request).await.unwrap();
        assert_eq!(within(self.second_host.input.recv::<ServerMsg>()).await.unwrap(),
            ServerMsg::Request { from: self.member.public(), kind });
    }

    async fn accept_second(&mut self, kind: Ask) -> ServerMsg {
        self.second_host.out.send(&ClientMsg::Accept { from: self.member.public(), kind }).await.unwrap();
        let reply = within(self.second_member.input.recv::<ServerMsg>()).await.unwrap();
        within(self.second_host.input.recv::<ServerMsg>()).await.unwrap();
        reply
    }

    async fn relay_second(&mut self) -> u32 {
        self.request_second(Ask::Relay).await;
        let ServerMsg::RelayReady { session, .. } = self.accept_second(Ask::Relay).await else {
            panic!("expected relay");
        };
        session
    }

    async fn revoke_first(&mut self) {
        self.first_host.out.send(&ClientMsg::Allow { members: Vec::new(), passes: Vec::new() }).await.unwrap();
        assert_eq!(within(self.first_host.input.recv::<ServerMsg>()).await.unwrap(), ServerMsg::Allowed);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_one_room_does_not_spend_another_rooms_pending_consent() {
    let mut rooms = TwoRooms::start().await;
    rooms.request_second(Ask::Introduce).await;
    rooms.revoke_first().await;
    assert!(matches!(rooms.accept_second(Ask::Introduce).await, ServerMsg::Punch { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_one_room_keeps_another_rooms_live_relay() {
    let mut rooms = TwoRooms::start().await;
    let session = rooms.relay_second().await;
    rooms.revoke_first().await;
    let packet = veil_rendezvous::protocol::session_packet(session, b"live second room");
    rooms.second_member._link.quic().send_datagram(packet.clone().into()).unwrap();
    assert_eq!(within(rooms.second_host._link.quic().read_datagram()).await.unwrap().as_ref(), packet);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registration_cannot_send_on_another_rooms_relay() {
    let mut rooms = TwoRooms::start().await;
    let session = rooms.relay_second().await;
    let packet = veil_rendezvous::protocol::session_packet(session, b"wrong room");
    rooms.first_member._link.quic().send_datagram(packet.into()).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(200), rooms.second_host._link.quic().read_datagram()).await.is_err(),
        "same identity on another registration cannot use this session");
    let mut stream = rooms.first_member._link.quic().open_uni().await.unwrap();
    stream.write_all(&veil_rendezvous::protocol::stream_header(session, 5).unwrap()).await.unwrap();
    stream.write_all(b"other").await.unwrap();
    stream.finish().unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(200), rooms.second_host._link.quic().accept_uni()).await.is_err(),
        "the stream fallback has the same registration boundary");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_departure_ends_only_its_own_room_and_guest_registrations() {
    let mut rooms = TwoRooms::start().await;
    let session = rooms.relay_second().await;
    rooms.first_host._link.close("host left");
    within(rooms.first_member._link.quic().closed()).await;
    let packet = veil_rendezvous::protocol::session_packet(session, b"surviving room");
    rooms.second_member._link.quic().send_datagram(packet.clone().into()).unwrap();
    assert_eq!(within(rooms.second_host._link.quic().read_datagram()).await.unwrap().as_ref(), packet);
}
