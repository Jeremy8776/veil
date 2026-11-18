//! Short invite links on localhost: a host stores a sealed invite, a guest fetches it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use veil_net::{Node, NodeAddr};
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::{
    Ask, FetchError, Refusal, RendezvousClient, RendezvousError, RendezvousServer, fetch_invite,
    room_tag, store_invite,
};
use veil_security::Identity;
use veil_security::invite::Invite;
use veil_security::short_invite::{SealedInvite, ShortInvite};

const ROOM: [u8; 16] = [7; 16];
const NOW: u64 = 1_800_000_000;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn start_server() -> NodeAddr {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node);
    let addr = NodeAddr {
        key: server.id(),
        addr: server.local_addr().unwrap(),
    };
    tokio::spawn(server.run());
    addr
}

struct Host {
    identity: Identity,
    node: Node,
    client: RendezvousClient,
}

/// A host with its room open on `server`.
async fn open_room(server: NodeAddr) -> Host {
    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let mut client = RendezvousClient::connect(&node, &[server.addr], server.key).await.unwrap();
    client
        .register(room_tag(&ROOM), identity.public(), None, Vec::new())
        .await
        .unwrap();
    Host { identity, node, client }
}

fn invite(host: &Identity, expires: u64) -> (Invite, String) {
    let invite = Invite::new(
        host,
        vec!["127.0.0.1:1".into()],
        None,
        expires,
        &mut UnwrapErr(SysRng),
    )
    .unwrap();
    let link = invite.to_link(host).unwrap();
    (invite, link)
}

fn short(link: &str, servers: &[SocketAddr]) -> (ShortInvite, SealedInvite) {
    ShortInvite::create(link, servers, &mut UnwrapErr(SysRng)).unwrap()
}

async fn store(host: &Host, server: NodeAddr, sealed: SealedInvite) -> Result<(), RendezvousError> {
    store_invite(&host.node, &[server.addr], server.key, room_tag(&ROOM), sealed).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_fetches_the_invite_with_only_the_short_link() {
    let server = start_server();
    let host = open_room(server).await;
    let (invite, full) = invite(&host.identity, NOW + 60);
    let (link, sealed) = short(&full, &[server.addr]);
    store(&host, server, sealed).await.unwrap();

    let pasted = ShortInvite::from_link(&link.to_link()).unwrap();
    assert_eq!(fetch_invite(&pasted, NOW).await.unwrap(), invite);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_host_of_an_open_room_can_store() {
    let server = start_server();
    let host = open_room(server).await;
    let (_, full) = invite(&host.identity, NOW + 60);

    let stranger = Node::bind(&Identity::generate(), localhost()).unwrap();
    let (_, sealed) = short(&full, &[server.addr]);
    let result =
        store_invite(&stranger, &[server.addr], server.key, room_tag(&ROOM), sealed).await;
    assert!(
        matches!(result, Err(RendezvousError::Refused(Refusal::NotAllowed))),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invites_go_when_the_host_leaves() {
    let server = start_server();
    let host = open_room(server).await;
    let (_, full) = invite(&host.identity, NOW + 60);
    let (link, sealed) = short(&full, &[server.addr]);
    store(&host, server, sealed).await.unwrap();
    assert!(fetch_invite(&link, NOW).await.is_ok());

    drop(host);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match fetch_invite(&link, NOW).await {
            Err(FetchError::NotFound) => break,
            Ok(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            other => panic!("still answered: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_or_unknown_invite_says_so() {
    let server = start_server();
    let host = open_room(server).await;
    let (_, full) = invite(&host.identity, NOW + 60);
    let (link, sealed) = short(&full, &[server.addr]);
    let (unknown, _) = short(&full, &[server.addr]);
    store(&host, server, sealed).await.unwrap();

    let late = fetch_invite(&link, NOW + 60).await.unwrap_err();
    assert!(late.to_string().contains("expired"), "{late}");
    assert_eq!(fetch_invite(&unknown, NOW).await, Err(FetchError::NotFound));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_real_node_wins_over_an_empty_one_at_another_address() {
    let (real, other) = (start_server(), start_server());
    let host = open_room(real).await;
    let (invite, full) = invite(&host.identity, NOW + 60);
    let (link, sealed) = short(&full, &[other.addr, real.addr]);
    store(&host, real, sealed).await.unwrap();
    assert_eq!(fetch_invite(&link, NOW).await.unwrap(), invite);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn storing_an_invite_leaves_the_hosts_place_and_requests_alone() {
    let server = start_server();
    let mut host = open_room(server).await;
    let guest_id = Identity::generate();
    host.client.allow(vec![guest_id.public()], Vec::new()).await.unwrap();
    let guest_node = Node::bind(&guest_id, localhost()).unwrap();
    let mut guest =
        RendezvousClient::connect(&guest_node, &[server.addr], server.key).await.unwrap();
    guest
        .register(room_tag(&ROOM), host.identity.public(), None, Vec::new())
        .await
        .unwrap();

    // The guest's request is waiting when the host's second link opens and closes.
    let me = host.identity.public();
    let asking = tokio::spawn(async move {
        let session = guest.open_relay(me).await;
        (guest, session)
    });
    let request = host.client.next_event().await.unwrap();
    assert_eq!(request, ServerMsg::Request { from: guest_id.public(), kind: Ask::Relay });
    let (_, full) = invite(&host.identity, NOW + 60);
    store(&host, server, short(&full, &[server.addr]).1).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    host.client.accept(guest_id.public(), Ask::Relay).await.unwrap();
    let (_guest, session) = asking.await.unwrap();
    assert!(session.is_ok(), "{session:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nobody_home_is_reported_as_unreachable() {
    let silent = std::net::UdpSocket::bind(localhost()).unwrap();
    let addr = silent.local_addr().unwrap();
    drop(silent);
    let (link, _) = short("veil:x", &[addr]);
    assert_eq!(fetch_invite(&link, NOW).await, Err(FetchError::Unreachable));
}
