//! Privacy policy remains enforced when the host proposes another media topology.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;
use tokio::net::UdpSocket;
use veil_call::{CallNode, HostHandle, Routes};
use veil_net::{Node, NodeAddr};
use veil_security::Identity;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_only_media_refuses_a_forwarder_before_any_direct_packet() {
    let identity = Identity::generate();
    let node = Node::bind(&identity, "127.0.0.1:0".parse().unwrap()).unwrap();
    let (host, _events) = HostHandle::start(&identity, [8; 16]).unwrap();
    let mut call = CallNode::start(&identity, node, [8; 16], None, host).await;
    call.set_relay_only(true);
    let mut media = call.media().unwrap();
    let sink = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = NodeAddr {
        key: Identity::generate().public(),
        addr: sink.local_addr().unwrap(),
    };
    let mut bytes = [0u8; 2048];
    tokio::select! {
        result = call.probe_forwarder(target, Vec::new()) => assert!(result.is_err()),
        _ = sink.recv_from(&mut bytes) => panic!("relay-only probe sent a direct packet"),
        _ = tokio::time::sleep(Duration::from_secs(1)) => panic!("probe policy refusal was delayed"),
    }
    tokio::select! {
        result = media.apply(Routes::Forwarder { addr: target, ticket: Vec::new() }) => {
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].0, target.key);
        }
        _ = sink.recv_from(&mut bytes) => panic!("relay-only media sent directly to a proposed forwarder"),
        _ = tokio::time::sleep(Duration::from_secs(1)) => panic!("policy refusal should precede network waits"),
    }
    media.redial();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), sink.recv_from(&mut bytes))
            .await
            .is_err()
    );
}
