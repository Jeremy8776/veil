//! A node started from configuration serves its roles on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use veil_forward::{ForwarderClient, RosterTicket};
use veil_net::{Node, NodeAddr};
use veil_node::{Config, start};
use veil_rendezvous::{Refusal, RendezvousClient, RendezvousError, room_tag};
use veil_security::Identity;

const BOTH: &str = r#"
identity_file = "unused.key"
[rendezvous]
listen = "127.0.0.1:0"
relay = false
[forwarder]
listen = "127.0.0.1:0"
"#;

async fn with_timeout<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f).await.expect("timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_node_serves_rendezvous_and_forwarding_with_one_key() {
    let identity = Identity::generate();
    let running = start(&Config::parse(BOTH).unwrap(), &identity).unwrap();
    let (rendezvous, forwarder) = (running.rendezvous.unwrap(), running.forwarder.unwrap());
    assert_eq!(rendezvous.key, identity.public());
    assert_eq!(forwarder.key, identity.public());
    assert_ne!(rendezvous.addr.port(), forwarder.addr.port());

    // The printed form is what an operator copies into an invite.
    let parsed: NodeAddr = rendezvous.to_string().parse().unwrap();
    assert_eq!(parsed, rendezvous);

    let room = room_tag(&[1; 16]);
    let member_id = Identity::generate();
    let member = Node::bind(&member_id, "127.0.0.1:0".parse().unwrap()).unwrap();
    let host = Identity::generate();
    let ticket = RosterTicket { room_tag: room, host: host.public(), epoch: 0, members: vec![member_id.public()] }
        .seal(&host)
        .unwrap();
    let mut client = with_timeout(RendezvousClient::connect(&member, &[parsed.addr], parsed.key))
        .await
        .unwrap();
    // The member opens its own room, then lets `other` in.
    with_timeout(client.register(room, member_id.public(), None, vec![])).await.unwrap();

    let other = Identity::generate();
    with_timeout(client.allow(vec![other.public()], vec![])).await.unwrap();
    let other_node = Node::bind(&other, "127.0.0.1:0".parse().unwrap()).unwrap();
    let mut other_client =
        with_timeout(RendezvousClient::connect(&other_node, &[parsed.addr], parsed.key))
            .await
            .unwrap();
    with_timeout(other_client.register(room, member_id.public(), None, vec![])).await.unwrap();
    assert!(matches!(
        with_timeout(client.open_relay(other.public())).await,
        Err(RendezvousError::Refused(Refusal::RelayDisabled))
    ));

    let joined = with_timeout(ForwarderClient::join(&member, &[forwarder.addr], forwarder.key, &ticket))
        .await
        .unwrap();
    assert_eq!(joined.members_at_join(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dial_must_pin_the_node_key() {
    let running = start(&Config::parse(BOTH).unwrap(), &Identity::generate()).unwrap();
    let forwarder = running.forwarder.unwrap();
    let member = Node::bind(&Identity::generate(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let wrong_key = Identity::generate().public();
    assert!(
        with_timeout(ForwarderClient::join(&member, &[forwarder.addr], wrong_key, &[]))
            .await
            .is_err()
    );
}
