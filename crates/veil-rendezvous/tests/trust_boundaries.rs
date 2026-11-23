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
