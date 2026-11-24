//! A rendezvous notice must not make a participant send packets it has no reason to send. Real
//! nodes on localhost, with a plain UDP socket standing in for the address being aimed at.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use veil_call::{CallNode, HostHandle};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::protocol::{ClientMsg, ServerMsg};
use veil_rendezvous::{RendezvousClient, RendezvousError, RendezvousServer, room_tag};
use veil_security::invite::{Invite, lobby_pass};
use veil_security::{Identity, PublicId};

const ROOM: [u8; 16] = [0x44; 16];
/// Long enough for the dials a notice starts to send their first packets.
const SETTLE: Duration = Duration::from_millis(1_500);

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn invite_from(host: &HostHandle, addr: SocketAddr, relay: Option<NodeAddr>) -> Invite {
    let relay = relay.map(|r| r.to_string());
    let link = host.invite(vec![addr.to_string()], relay, now() + 600).await.unwrap();
    Invite::from_link(&link, now()).unwrap()
}

/// How many datagrams reach `sink` within [`SETTLE`].
async fn datagrams_at(sink: &UdpSocket) -> usize {
    let mut buffer = [0u8; 2_048];
    let mut count = 0;
    let deadline = tokio::time::Instant::now() + SETTLE;
    while let Ok(Ok(_)) = tokio::time::timeout_at(deadline, sink.recv_from(&mut buffer)).await {
        count += 1;
    }
    count
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_introductions_never_dial_an_unverified_claimed_address() {
    let server = RendezvousServer::new(Node::bind(&Identity::generate(), localhost()).unwrap());
    let rendezvous = NodeAddr { key: server.id(), addr: server.local_addr().unwrap() };
    tokio::spawn(server.run());

    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let addr = node.local_addr().unwrap();
    let (host, _events) = HostHandle::start(&identity, ROOM).unwrap();
    let _host_node = CallNode::start(&identity, node, ROOM, Some(rendezvous), host.clone()).await;
    let invite = invite_from(&host, addr, Some(rendezvous)).await;

    // An invite holder lists an address of its choice and asks again and again.
    let sink = UdpSocket::bind(localhost()).await.unwrap();
    let holder = Node::bind(&Identity::generate(), localhost()).unwrap();
    let mut client = RendezvousClient::connect(&holder, &[rendezvous.addr], rendezvous.key)
        .await
        .unwrap();
    let pass = lobby_pass(&invite.join_token);
    let locals = vec![sink.local_addr().unwrap()];
    let mut registered = false;
    for _ in 0..100 {
        let tag = room_tag(&invite.room_id);
        match client.register(tag, invite.host, Some(pass), locals.clone()).await {
            Ok(_) => {
                registered = true;
                break;
            }
            // The host's list is on its way.
            Err(RendezvousError::Refused(_)) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(e) => panic!("register: {e:?}"),
        }
    }
    assert!(registered, "the rendezvous never let the invite holder in");
    for _ in 0..20 {
        client.introduce(invite.host).await.unwrap();
    }

    let seen = datagrams_at(&sink).await;
    assert_eq!(seen, 0, "claimed addresses are not authenticated endpoints");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_punch_nobody_agreed_to_sends_nothing() {
    // A rendezvous that tells every participant to dial, unasked.
    let rendezvous_id = Identity::generate();
    let rendezvous = Node::bind(&rendezvous_id, localhost()).unwrap();
    let address = NodeAddr { key: rendezvous_id.public(), addr: rendezvous.local_addr().unwrap() };
    let sink = UdpSocket::bind(localhost()).await.unwrap();
    let target = sink.local_addr().unwrap();
    let hostile = tokio::spawn(async move {
        let link = rendezvous.accept().await.unwrap().unwrap();
        let (mut out, mut input) = link.accept_control().await.unwrap().split();
        let ClientMsg::Register { .. } = input.recv::<ClientMsg>().await.unwrap() else {
            panic!("expected a registration");
        };
        out.send(&ServerMsg::Registered { observed: link.remote_addr() }).await.unwrap();
        for n in 0..5 {
            let punch = ServerMsg::Punch {
                peer: PublicId([n; 32]),
                addrs: vec![target],
                delay_ms: 0,
            };
            out.send(&punch).await.unwrap();
        }
        std::future::pending::<()>().await;
        drop((link, rendezvous));
    });

    let host_identity = Identity::generate();
    let (host, _events) = HostHandle::start(&host_identity, ROOM).unwrap();
    let invite = invite_from(&host, localhost(), None).await;
    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let guest = CallNode::for_invite_via(&identity, node, &invite, Some(address)).await;
    assert!(guest.has_rendezvous());

    assert_eq!(datagrams_at(&sink).await, 0);
    hostile.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn even_consented_helper_reflection_cannot_target_an_unknown_endpoint() {
    let rendezvous_id = Identity::generate();
    let rendezvous = Node::bind(&rendezvous_id, localhost()).unwrap();
    let address = NodeAddr { key: rendezvous_id.public(), addr: rendezvous.local_addr().unwrap() };
    let sink = UdpSocket::bind(localhost()).await.unwrap();
    let target = sink.local_addr().unwrap();
    let stranger = Identity::generate().public();
    let (sent, observed) = tokio::sync::oneshot::channel();
    let hostile = tokio::spawn(async move {
        let link = rendezvous.accept().await.unwrap().unwrap();
        let (mut out, mut input) = link.accept_control().await.unwrap().split();
        assert!(matches!(input.recv::<ClientMsg>().await.unwrap(), ClientMsg::Register { .. }));
        out.send(&ServerMsg::Registered { observed: link.remote_addr() }).await.unwrap();
        assert!(matches!(input.recv::<ClientMsg>().await.unwrap(), ClientMsg::Allow { .. }));
        out.send(&ServerMsg::Allowed).await.unwrap();
        out.send(&ServerMsg::Request { from: stranger, kind: veil_rendezvous::Ask::Introduce }).await.unwrap();
        loop {
            match input.recv::<ClientMsg>().await.unwrap() {
                ClientMsg::Accept { from, kind: veil_rendezvous::Ask::Introduce } if from == stranger => break,
                ClientMsg::Allow { .. } => out.send(&ServerMsg::Allowed).await.unwrap(),
                _ => {},
            }
        }
        out.send(&ServerMsg::Punch { peer: stranger, addrs: vec![target], delay_ms: 0 }).await.unwrap();
        sent.send(()).unwrap();
        std::future::pending::<()>().await;
        drop((link, rendezvous));
    });
    let identity = Identity::generate();
    let (host, _events) = HostHandle::start(&identity, ROOM).unwrap();
    let _call = CallNode::start(&identity, Node::bind(&identity, localhost()).unwrap(), ROOM, Some(address), host).await;
    tokio::time::timeout(Duration::from_secs(2), observed).await.unwrap().unwrap();
    assert_eq!(datagrams_at(&sink).await, 0, "consent to the identity does not prove the helper's address");
    hostile.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_requested_introduction_cannot_send_to_an_unknown_reflected_endpoint() {
    let rendezvous_id = Identity::generate();
    let rendezvous = Node::bind(&rendezvous_id, localhost()).unwrap();
    let address = NodeAddr { key: rendezvous_id.public(), addr: rendezvous.local_addr().unwrap() };
    let sink = UdpSocket::bind(localhost()).await.unwrap();
    let target = sink.local_addr().unwrap();
    let host_identity = Identity::generate();
    let host_key = host_identity.public();
    let hostile = tokio::spawn(async move {
        let link = rendezvous.accept().await.unwrap().unwrap();
        let (mut out, mut input) = link.accept_control().await.unwrap().split();
        assert!(matches!(input.recv::<ClientMsg>().await.unwrap(), ClientMsg::Register { .. }));
        out.send(&ServerMsg::Registered { observed: link.remote_addr() }).await.unwrap();
        loop {
            match input.recv::<ClientMsg>().await.unwrap() {
                ClientMsg::Introduce { peer } => {
                    assert_eq!(peer, host_key);
                    out.send(&ServerMsg::Punch { peer, addrs: vec![target], delay_ms: 0 }).await.unwrap();
                }
                ClientMsg::OpenRelay { .. } => out.send(&ServerMsg::Refused(veil_rendezvous::Refusal::RelayDisabled)).await.unwrap(),
                _ => {},
            }
        }
    });
    let invite = Invite { version: 1, room_id: ROOM, host: host_key, addrs: Vec::new(),
        relay: Some(address.to_string()), expires_unix: now() + 60, join_token: [2; 16] };
    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let call = CallNode::for_invite(&identity, node, &invite).await;
    let mut bytes = [0u8; 2048];
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(3), veil_call::join(&call, &invite)) => assert!(result.unwrap().is_err()),
        _ = sink.recv_from(&mut bytes) => panic!("requested introduction sent to a helper-invented target"),
    }
    assert_eq!(datagrams_at(&sink).await, 0);
    hostile.abort();
}
