//! Room slots (GHSA-rgc8-82r3-49vh): one address block cannot hold every room, and a room
//! that keeps a single member gives its slot back.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_forward::{ForwardError, ForwarderClient, ForwarderServer, Limits, Refusal, RosterTicket};
use veil_net::Node;
use veil_rendezvous::room_tag;
use veil_security::{Identity, PublicId};

fn addr(ip: &str) -> SocketAddr {
    format!("{ip}:0").parse().unwrap()
}

fn start(limits: Limits) -> (SocketAddr, PublicId) {
    let node = Node::bind(&Identity::generate(), addr("127.0.0.1")).unwrap();
    let server = ForwarderServer::new(node, limits);
    let found = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());
    found
}

struct Member {
    _node: Node,
    client: ForwarderClient,
}

/// Joins room `id` from `from` as a fresh device under a ticket a fresh host signed.
async fn join_from(
    forwarder: (SocketAddr, PublicId),
    from: &str,
    id: u8,
    host: &Identity,
) -> Result<Member, ForwardError> {
    let device = Identity::generate();
    let ticket = RosterTicket {
        room_tag: room_tag(&[id; 16]),
        host: host.public(),
        epoch: 0,
        members: vec![device.public()],
    }
    .seal(host)
    .unwrap();
    let node = Node::bind(&device, addr(from)).unwrap();
    let client = ForwarderClient::join(&node, &[forwarder.0], forwarder.1, &ticket).await?;
    Ok(Member { _node: node, client })
}

fn is_full<T>(result: &Result<T, ForwardError>) -> bool {
    matches!(result, Err(ForwardError::Refused(Refusal::Full)))
}

async fn closed(member: &Member) -> bool {
    tokio::time::timeout(Duration::from_secs(2), member.client.link().quic().closed())
        .await
        .is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_block_cannot_take_every_room() {
    let forwarder = start(Limits { max_rooms: 8, max_rooms_per_block: 4, ..Limits::default() });
    let host = Identity::generate();
    let mut held = Vec::new();
    for id in 0..4 {
        held.push(join_from(forwarder, "127.0.0.1", id, &host).await.unwrap());
    }
    assert!(is_full(&join_from(forwarder, "127.0.0.1", 4, &host).await));
    // Another address block still opens rooms.
    let other = join_from(forwarder, "127.0.0.2", 5, &host).await;
    assert!(other.is_ok(), "another block was refused");
    // Joining a room that is already open does not count as opening one.
    assert!(join_from(forwarder, "127.0.0.1", 0, &host).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lone_member_is_let_go_and_its_slot_comes_back() {
    let grace = Duration::from_millis(300);
    let forwarder = start(Limits { max_rooms_per_block: 1, alone_grace: grace, ..Limits::default() });
    let host = Identity::generate();
    let alone = join_from(forwarder, "127.0.0.1", 1, &host).await.unwrap();
    assert!(is_full(&join_from(forwarder, "127.0.0.1", 2, &host).await));
    assert!(closed(&alone).await, "a member alone past the grace time should be let go");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(join_from(forwarder, "127.0.0.1", 2, &host).await.is_ok(), "the slot was not given back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_room_of_two_outlives_the_grace_time() {
    let grace = Duration::from_millis(300);
    let forwarder = start(Limits { alone_grace: grace, ..Limits::default() });
    let host = Identity::generate();
    // Both members on one ticket's room: the second join lists only itself, which the room
    // accepts at the same epoch.
    let a = join_from(forwarder, "127.0.0.1", 3, &host).await.unwrap();
    let b = join_from(forwarder, "127.0.0.1", 3, &host).await.unwrap();
    tokio::time::sleep(grace * 3).await;
    assert!(a.client.link().quic().close_reason().is_none(), "a was let go");
    assert!(b.client.link().quic().close_reason().is_none(), "b was let go");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_forwarder_closes_its_registered_links() {
    let node = Node::bind(&Identity::generate(), addr("127.0.0.1")).unwrap();
    let server = ForwarderServer::new(node, Limits::default());
    let forwarder = (server.local_addr().unwrap(), server.id());
    let task = tokio::spawn(server.run());
    let host = Identity::generate();
    let member = join_from(forwarder, "127.0.0.1", 9, &host).await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(closed(&member).await, "stopping the server must stop its member workers and links");
}
