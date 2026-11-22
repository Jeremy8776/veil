//! A node shows its key only to diallers that already know it, unless it is opened.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::Node;
use veil_security::Identity;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn listening(identity: &Identity) -> (SocketAddr, std::sync::Arc<Node>) {
    let node = std::sync::Arc::new(Node::bind(identity, localhost()).unwrap());
    let addr = node.local_addr().unwrap();
    let accepting = node.clone();
    tokio::spawn(async move { while let Some(Ok(_link)) = accepting.accept().await {} });
    (addr, node)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stranger_without_the_key_learns_nothing() {
    let (addr, _node) = listening(&Identity::generate());
    let scanner = Node::bind(&Identity::generate(), localhost()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), scanner.connect_unverified(&[addr]))
        .await
        .expect("a refused handshake ends promptly");
    assert!(result.is_err(), "a closed node completed a handshake with a stranger");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dialler_that_knows_the_key_gets_in() {
    let host = Identity::generate();
    let (addr, _node) = listening(&host);
    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = guest.connect(&[addr], host.public()).await.unwrap();
    assert_eq!(link.peer(), host.public());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opened_service_answers_strangers() {
    let service = Identity::generate();
    let (addr, node) = listening(&service);
    node.admit_strangers();
    let visitor = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = visitor.connect_unverified(&[addr]).await.unwrap();
    assert_eq!(link.peer(), service.public());
}
