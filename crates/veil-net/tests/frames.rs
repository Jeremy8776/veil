//! `FrameReader`: a stalled or malformed frame stream does not hold back the next frame.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::{FrameReader, Node};
use veil_security::Identity;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_frame_does_not_block_the_next() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();

    let receiver = tokio::spawn(async move {
        let link = host.accept().await.unwrap().unwrap();
        let mut frames = FrameReader::new(&link);
        let got = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("the good frame should not wait for the stalled one")
            .unwrap();
        (got, link)
    });

    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = guest.connect(&[host_addr], host_id.public()).await.unwrap();

    // A stream that claims 1 MB, sends 3 bytes and is never finished.
    let mut stalled = link.quic().open_uni().await.unwrap();
    let mut head = [0u8; 8];
    head[4..].copy_from_slice(&1_000_000u32.to_be_bytes());
    stalled.write_all(&head).await.unwrap();
    stalled.write_all(b"abc").await.unwrap();
    // A stream that sends more than its header says.
    let mut long = link.quic().open_uni().await.unwrap();
    let mut head = [0u8; 8];
    head[4..].copy_from_slice(&2u32.to_be_bytes());
    long.write_all(&head).await.unwrap();
    long.write_all(b"too long").await.unwrap();
    long.finish().unwrap();

    link.send_frame(7, b"good frame").await.unwrap();
    let ((id, sealed), _link) = receiver.await.unwrap();
    assert_eq!((id, sealed.as_slice()), (7, &b"good frame"[..]));
    drop(stalled);
}
