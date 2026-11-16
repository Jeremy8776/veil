//! A handshake that never finishes must not hold up other links.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use veil_net::Node;
use veil_security::Identity;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Forwards the client's packets to `server` and drops every reply, so the server starts a
/// handshake that can never finish. Returns the address to dial.
async fn black_hole(server: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind(localhost()).await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        while let Ok((len, from)) = socket.recv_from(&mut buf).await {
            if from != server {
                let _ = socket.send_to(&buf[..len], server).await;
            }
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_handshake_does_not_block_other_links() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();

    // The stalled dial: its Initial reaches the host, the host's answer never comes back.
    let stalled = Node::bind(&Identity::generate(), localhost()).unwrap();
    let hole = black_hole(host_addr).await;
    let host_key = host_id.public();
    tokio::spawn(async move {
        let _ = stalled.connect(&[hole], host_key).await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let guest_id = Identity::generate();
    let guest = Node::bind(&guest_id, localhost()).unwrap();
    let dial =
        tokio::spawn(async move { guest.connect(&[host_addr], host_key).await.map(|_| guest) });
    let accepted = tokio::time::timeout(Duration::from_secs(3), host.accept())
        .await
        .expect("the stalled handshake held up the accept loop")
        .unwrap()
        .unwrap();
    assert_eq!(accepted.peer(), guest_id.public());
    assert!(dial.await.unwrap().is_ok());
}
