//! A host's short link, made through its `CallNode` and followed the way a guest does.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;

use veil_call::{CallError, CallNode, HostHandle, RelayAddrs};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{RendezvousServer, fetch_invite, room_tag};
use veil_security::Identity;
use veil_security::short_invite::ShortInvite;

const ROOM: [u8; 16] = [9; 16];
const NOW: u64 = 1_800_000_000;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// The server a host runs for its own call, which serves this room only.
fn own_server(host: &Identity) -> NodeAddr {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node).only_room(room_tag(&ROOM), host.public());
    let addr = NodeAddr {
        key: server.id(),
        addr: server.local_addr().unwrap(),
    };
    tokio::spawn(server.run());
    addr
}

async fn hosting(identity: &Identity, rendezvous: Option<NodeAddr>) -> (HostHandle, CallNode, tokio::sync::mpsc::Receiver<veil_call::HostEvent>) {
    let (host, events) = HostHandle::start(identity, ROOM).unwrap();
    let node = Node::bind(identity, localhost()).unwrap();
    let call = CallNode::start(identity, node, ROOM, rendezvous, host.clone()).await;
    (host, call, events)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_gets_the_hosts_invite_from_the_short_link() {
    let identity = Identity::generate();
    let server = own_server(&identity);
    let (host, call, _events) = hosting(&identity, Some(server)).await;
    let relay = RelayAddrs::from(server);
    let full = host
        .invite(vec!["127.0.0.1:1".into()], Some(relay.to_string()), NOW + 60)
        .await
        .unwrap();

    let link = call.shorten_invite(&relay, &full).await.unwrap();
    assert!(link.len() < full.len() / 3, "{} from {}", link.len(), full.len());
    let invite = fetch_invite(&ShortInvite::from_link(&link).unwrap(), NOW).await.unwrap();
    assert_eq!(invite.host, identity.public());
    assert_eq!(invite.room_id, ROOM);
    assert_eq!(invite.relay, Some(relay.to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_rendezvous_there_is_no_short_link() {
    let identity = Identity::generate();
    let (_host, call, _events) = hosting(&identity, None).await;
    let relay = RelayAddrs::from(NodeAddr {
        key: Identity::generate().public(),
        addr: "127.0.0.1:1".parse().unwrap(),
    });
    let result = call.shorten_invite(&relay, "veil:x").await;
    assert!(matches!(result, Err(CallError::Protocol(_))), "{result:?}");
}
