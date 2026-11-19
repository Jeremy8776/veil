//! Transport limits (GHSA-g7j3-q88p-63cf): a peer cannot make a node hold more than a small,
//! fixed amount per link in streams the node never reads, and real frames still cross.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::Node;
use veil_security::Identity;

/// `LINK_WINDOW` in `endpoint.rs`.
const LINK_WINDOW: usize = 4 * 1024 * 1024;
/// Room for QUIC framing and the credit quinn grants in steps.
const SLACK: usize = 256 * 1024;
const BLOCKED: Duration = Duration::from_millis(150);

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Writes to `stream` until the peer stops granting credit; returns the bytes it took.
async fn fill(stream: &mut quinn::SendStream) -> usize {
    let chunk = vec![0u8; 64 * 1024];
    let mut taken = 0;
    while let Ok(Ok(n)) = tokio::time::timeout(BLOCKED, stream.write(&chunk)).await {
        taken += n;
    }
    taken
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unread_streams_hold_no_more_than_the_link_window() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();
    // The host accepts the link and then reads nothing on it.
    let held = tokio::spawn(async move {
        let link = host.accept().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop((link, host));
    });

    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = guest.connect(&[host_addr], host_id.public()).await.unwrap();
    let conn = link.quic();

    let mut streams = Vec::new();
    for _ in 0..2 {
        streams.push(conn.open_bi().await.unwrap().0);
    }
    assert!(
        tokio::time::timeout(BLOCKED, conn.open_bi()).await.is_err(),
        "a third bidirectional stream should wait for the host"
    );
    for _ in 0..40 {
        streams.push(conn.open_uni().await.unwrap());
    }

    let mut total = 0;
    for stream in &mut streams {
        total += fill(stream).await;
    }
    assert!(
        total <= LINK_WINDOW + SLACK,
        "the host took {total} unread bytes on one link"
    );
    held.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_larger_than_the_windows_still_crosses() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();
    let receiver = tokio::spawn(async move {
        let link = host.accept().await.unwrap().unwrap();
        link.recv_frame().await.unwrap()
    });

    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = guest.connect(&[host_addr], host_id.public()).await.unwrap();
    let frame: Vec<u8> = (0..6_000_000u32).map(|i| (i % 251) as u8).collect();
    link.send_frame(9, &frame).await.unwrap();

    let (id, got) = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("the large frame did not arrive")
        .unwrap();
    assert_eq!(id, 9);
    assert_eq!(got, frame);
}
