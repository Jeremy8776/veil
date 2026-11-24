//! Endpoint evidence comes from this node's authenticated transport, never a helper hint.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use veil_net::Node;
use veil_security::Identity;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endpoint_evidence_is_bound_to_successful_pinned_or_incoming_links_and_one_node() {
    let a = Identity::generate();
    let b = Identity::generate();
    let node_a = Node::bind(&a, "127.0.0.1:0".parse().unwrap()).unwrap();
    let node_b = Node::bind(&b, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr_a = node_a.local_addr().unwrap();
    let addr_b = node_b.local_addr().unwrap();
    assert!(!node_a.authenticated_at(b.public(), addr_b));
    let outgoing = node_a.connect(&[addr_b], b.public()).await.unwrap();
    let incoming = node_b.accept().await.unwrap().unwrap();
    assert!(node_a.authenticated_at(b.public(), addr_b));
    assert!(node_b.authenticated_at(a.public(), addr_a));
    assert!(!node_a.authenticated_at(a.public(), addr_b));
    assert!(!node_a.authenticated_at(b.public(), addr_a));
    let isolated = Node::bind(&a, "127.0.0.1:0".parse().unwrap()).unwrap();
    assert!(!isolated.authenticated_at(b.public(), addr_b));
    node_b.admit_strangers();
    let unverified = isolated.connect_unverified(&[addr_b]).await.unwrap();
    assert!(
        !isolated.authenticated_at(b.public(), addr_b),
        "unverified outgoing fetch is not evidence"
    );
    drop((incoming, outgoing, unverified));
}
