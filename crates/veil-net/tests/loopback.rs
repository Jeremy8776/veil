//! Two nodes on localhost: pinned handshake, control messages and encrypted media.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::control::Control;
use veil_net::{NetError, Node};
use veil_security::Identity;
use veil_security::frame::{FrameReceiver, FrameSender};

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_link_carries_control_messages() {
    let host_id = Identity::generate();
    let guest_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let guest = Node::bind(&guest_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();
    let guest_key = guest_id.public();

    let server = tokio::spawn(async move {
        let link = host.accept().await.unwrap().unwrap();
        assert_eq!(link.peer(), guest_key);
        let mut ctl = link.accept_control().await.unwrap();
        let hello = ctl.recv().await.unwrap();
        assert_eq!(
            hello,
            Control::Hello {
                room_id: [1; 16],
                join_token: [2; 16]
            }
        );
        ctl.send(&Control::Admitted).await.unwrap();
        // Keep the link open until the guest has read the reply.
        let _ = ctl.recv().await;
    });

    let link = guest.connect(&[host_addr], host_id.public()).await.unwrap();
    assert_eq!(link.peer(), host_id.public());
    let mut ctl = link.open_control().await.unwrap();
    ctl.send(&Control::Hello {
        room_id: [1; 16],
        join_token: [2; 16],
    })
    .await
    .unwrap();
    assert_eq!(ctl.recv().await.unwrap(), Control::Admitted);
    link.close("done");
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impostor_is_refused() {
    let real_host = Identity::generate();
    let impostor = Identity::generate();
    let node = Node::bind(&impostor, localhost()).unwrap();
    let addr = node.local_addr().unwrap();
    tokio::spawn(async move { while node.accept().await.is_some() {} });

    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let result = guest.connect(&[addr], real_host.public()).await;
    assert!(
        matches!(result, Err(NetError::Connect(_))),
        "got {:?}",
        result.err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_presented_wrong_key_retains_typed_identity_evidence() {
    let impostor = Node::bind(&Identity::generate(), localhost()).unwrap();
    // Public helpers answer even an unknown knock. Closed call ports may reveal no key.
    impostor.admit_strangers();
    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let result = guest
        .connect(
            &[impostor.local_addr().unwrap()],
            Identity::generate().public(),
        )
        .await;
    assert!(
        matches!(result, Err(NetError::Identity)),
        "got {:?}",
        result.err()
    );
    impostor.close();
    guest.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_reachable_address_wins() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let good = host.local_addr().unwrap();
    tokio::spawn(async move { while let Some(Ok(_link)) = host.accept().await {} });

    let dead: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = tokio::time::timeout(
        Duration::from_secs(5),
        guest.connect(&[dead, good], host_id.public()),
    )
    .await
    .expect("should not wait for the dead address")
    .unwrap();
    assert_eq!(link.remote_addr(), good);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_keyframe_crosses_as_datagrams() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();
    let epoch_secret = [7u8; 32];
    // A keyframe-sized payload: about 300 KB.
    let frame: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let expected = frame.clone();

    let receiver = tokio::spawn(async move {
        let link = host.accept().await.unwrap().unwrap();
        let mut keys = FrameReceiver::new(&epoch_secret, 1).unwrap();
        let mut frames = Vec::new();
        for _ in 0..3 {
            let (frame_id, sealed) = link.recv_frame().await.unwrap();
            let (sender, plain) = keys.decrypt(&sealed).unwrap();
            assert_eq!(sender, 3);
            frames.push((frame_id, plain));
        }
        frames
    });

    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = guest.connect(&[host_addr], host_id.public()).await.unwrap();
    let mut seal = FrameSender::new(&epoch_secret, 1, 3).unwrap();
    for id in 0..3 {
        let sealed = seal.encrypt(&frame).unwrap();
        assert!(!sealed.windows(64).any(|w| w == &frame[1_000..1_064]));
        link.send_frame(id, &sealed).await.unwrap();
    }

    let got = tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .expect("frames did not arrive")
        .unwrap();
    assert_eq!(got.len(), 3);
    for (i, (id, plain)) in got.into_iter().enumerate() {
        assert_eq!(id as usize, i);
        assert_eq!(plain, expected);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_packet_crosses_as_one_datagram() {
    let host_id = Identity::generate();
    let host = Node::bind(&host_id, localhost()).unwrap();
    let host_addr = host.local_addr().unwrap();
    let receiver = tokio::spawn(async move {
        let link = host.accept().await.unwrap().unwrap();
        link.recv_datagram().await.unwrap()
    });
    let guest = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = guest.connect(&[host_addr], host_id.public()).await.unwrap();
    link.send_datagram(&[9u8; 160]).unwrap();
    assert!(link.send_datagram(&vec![0u8; 70_000]).is_err());
    let got = tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .expect("audio did not arrive")
        .unwrap();
    assert_eq!(&got[..], &[9u8; 160][..]);
}

/// A dual-stack node is dialled over IPv4 and over IPv6 on one socket, and an IPv4 peer
/// shows as plain IPv4.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dual_stack_node_takes_both_families() {
    let host_id = Identity::generate();
    let host = Node::bind_any(&host_id, 0).unwrap();
    if !host.ipv6() {
        eprintln!("no IPv6 on this machine; skipped");
        return;
    }
    let port = host.local_addr().unwrap().port();
    tokio::spawn(async move { while host.accept().await.is_some() {} });

    let v4 = Node::bind(&Identity::generate(), localhost()).unwrap();
    let link = v4
        .connect(
            &[SocketAddr::from(([127, 0, 0, 1], port))],
            host_id.public(),
        )
        .await
        .unwrap();
    assert!(link.remote_addr().is_ipv4());

    let v6 = Node::bind_any(&Identity::generate(), 0).unwrap();
    let link = v6
        .connect(
            &[
                SocketAddr::from(([127, 0, 0, 1], port)),
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
            ],
            host_id.public(),
        )
        .await
        .unwrap();
    assert!(
        link.remote_addr().is_ipv6(),
        "IPv6 should win: {}",
        link.remote_addr()
    );
}
