//! The diagnostics surface preserves authenticated refusal separately from reachability.
#![allow(clippy::expect_used)]

use veil_call::{CallNode, HostHandle, node::HelperIssue};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{Refusal, RendezvousServer};
use veil_security::Identity;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusing_helper_preserves_authenticated_host_policy_evidence() {
    let socket = Node::bind(&Identity::generate(), "127.0.0.1:0".parse().expect("address")).expect("socket");
    let addr = NodeAddr { key: socket.id(), addr: socket.local_addr().expect("address") };
    let server = RendezvousServer::new(socket).only_hosts([]);
    let task = tokio::spawn(server.run());
    let identity = Identity::generate();
    let node = Node::bind(&identity, "127.0.0.1:0".parse().expect("address")).expect("socket");
    let (host, _events) = HostHandle::start(&identity, [1;16]).expect("host");
    let call = CallNode::start(&identity, node, [1;16], Some(addr), host).await;
    assert!(!call.has_rendezvous(), "refusal is not successful registration");
    assert_eq!(call.rendezvous_refusal(), Some(Refusal::NotAllowed));
    assert_eq!(call.helper_issue(), Some(HelperIssue::Refused(Refusal::NotAllowed)));
    task.abort();
}
