//! A link that joins no room cannot stay open on a rendezvous server.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::Node;
use veil_rendezvous::{RendezvousClient, RendezvousServer};
use veil_security::Identity;

const WAIT: Duration = Duration::from_millis(200);

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test]
async fn only_links_that_register_outlast_the_wait() {
    let server = RendezvousServer::new(Node::bind(&Identity::generate(), localhost()).unwrap())
        .with_unregistered_wait(WAIT);
    let (addr, key) = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());

    let idle_node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let mut idle = RendezvousClient::connect(&idle_node, &[addr], key).await.unwrap();
    // The server sees the control stream only once something is sent on it.
    assert_eq!(idle.get_invite([0; 32]).await.unwrap(), None);

    let host = Identity::generate();
    let host_node = Node::bind(&host, localhost()).unwrap();
    let mut member = RendezvousClient::connect(&host_node, &[addr], key).await.unwrap();
    member.register([1; 32], host.public(), None, Vec::new()).await.unwrap();

    let closed = tokio::time::timeout(Duration::from_secs(10), idle.next_event()).await;
    assert!(matches!(closed, Ok(None)), "the unregistered link is closed");
    let still_open = tokio::time::timeout(WAIT * 3, member.next_event()).await;
    assert!(still_open.is_err(), "the registered link stays open");
}
