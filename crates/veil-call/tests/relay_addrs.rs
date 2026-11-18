//! An invite that names the rendezvous at several addresses, over real QUIC on localhost: a
//! joiner still registers when one of them never answers, as a LAN address does from outside.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use veil_call::{CallNode, HostHandle, RelayAddrs};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::RendezvousServer;
use veil_security::Identity;
use veil_security::invite::Invite;

const ROOM: [u8; 16] = [9; 16];

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_joiner_registers_when_one_rendezvous_address_is_silent() {
    let server = RendezvousServer::new(Node::bind(&Identity::generate(), localhost()).unwrap());
    let live = NodeAddr {
        key: server.id(),
        addr: server.local_addr().unwrap(),
    };
    tokio::spawn(server.run());
    // Bound and never read, so packets to it get no answer and no error.
    let silent = UdpSocket::bind(localhost()).unwrap();

    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let host_addr = node.local_addr().unwrap();
    let (host, _events) = HostHandle::start(&identity, ROOM).unwrap();
    let call = CallNode::start(&identity, node, ROOM, Some(live), host.clone()).await;
    assert!(call.has_rendezvous());

    let relay = RelayAddrs {
        key: live.key,
        addrs: vec![silent.local_addr().unwrap(), live.addr],
    };
    let link = host
        .invite(
            vec![host_addr.to_string()],
            Some(relay.to_string()),
            now() + 600,
        )
        .await
        .unwrap();
    let invite = Invite::from_link(&link, now()).unwrap();

    let guest = Identity::generate();
    let node = Node::bind(&guest, localhost()).unwrap();
    let joining = CallNode::for_invite(&guest, node, &invite);
    let joined = tokio::time::timeout(Duration::from_secs(10), joining)
        .await
        .expect("the silent address held up the dial");
    assert!(joined.has_rendezvous());
}
