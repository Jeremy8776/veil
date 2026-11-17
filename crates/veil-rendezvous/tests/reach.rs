//! Reachability checks on localhost: the server dials the asker back from another socket.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;

use veil_net::{Node, NodeAddr};
use veil_rendezvous::{Refusal, RendezvousError, RendezvousServer, check_reach};
use veil_security::Identity;

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

#[tokio::test]
async fn a_listening_node_is_reached_at_its_own_port() {
    let server = start_server();
    let me = Node::bind(&Identity::generate(), localhost()).unwrap();
    let port = me.local_addr().unwrap().port();
    let reach = check_reach(&me, &server, port).await.unwrap();
    assert!(reach.open, "the node's own port answers");
    assert_eq!(reach.addr, SocketAddr::from(([127, 0, 0, 1], port)));
}

#[tokio::test]
async fn a_silent_port_or_another_key_is_not_open() {
    let server = start_server();
    let me = Node::bind(&Identity::generate(), localhost()).unwrap();
    // Another machine's key at the port: the dial pins mine, so it fails.
    let other = Node::bind(&Identity::generate(), localhost()).unwrap();
    let other_port = other.local_addr().unwrap().port();
    // A port nothing listens on.
    let silent = std::net::UdpSocket::bind(localhost()).unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    drop(silent);

    let (wrong_key, nobody) = tokio::join!(
        check_reach(&me, &server, other_port),
        check_reach(&me, &server, silent_port),
    );
    assert!(
        !wrong_key.unwrap().open,
        "another key at the port is not me"
    );
    assert!(!nobody.unwrap().open, "nothing listens there");
}

#[tokio::test]
async fn port_zero_is_refused() {
    let server = start_server();
    let me = Node::bind(&Identity::generate(), localhost()).unwrap();
    let result = check_reach(&me, &server, 0).await;
    assert!(
        matches!(result, Err(RendezvousError::Refused(Refusal::BadPort))),
        "{result:?}"
    );
}
