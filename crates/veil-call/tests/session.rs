//! Host and members over real QUIC links on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use veil_call::{CallError, CallNode, EpochKeys, HostEvent, HostHandle, MemberEvent, MemberHandle, join};
use veil_net::Node;
use veil_net::control::RefuseReason;
use veil_security::invite::Invite;
use veil_security::{Identity, PublicId, SecurityError};

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), f).await.expect("timed out")
}

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Member {
    _node: CallNode,
    device: PublicId,
    handle: MemberHandle,
    keys: EpochKeys,
}

struct Call {
    _node: CallNode,
    host: HostHandle,
    events: UnboundedReceiver<HostEvent>,
    addr: SocketAddr,
    keys: EpochKeys,
}

async fn start_call() -> Call {
    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let addr = node.local_addr().unwrap();
    let (host, mut events) = HostHandle::start(&identity, [4; 16]).unwrap();
    let node = CallNode::start(&identity, node, [4; 16], None, host.clone()).await;
    let keys = next_keys(&mut events).await;
    assert_eq!(keys.epoch, 0);
    Call { _node: node, host, events, addr, keys }
}

async fn next_keys(events: &mut UnboundedReceiver<HostEvent>) -> EpochKeys {
    loop {
        match within(events.recv()).await.unwrap() {
            HostEvent::Keys(keys) => return keys,
            HostEvent::Error(e) => panic!("host error: {e}"),
            _ => {}
        }
    }
}

async fn member_keys(member: &mut MemberHandle) -> EpochKeys {
    match within(member.events.recv()).await.unwrap() {
        MemberEvent::Keys(keys) => keys,
        other => panic!("expected keys, got {other:?}"),
    }
}

impl Call {
    async fn invite(&self) -> Invite {
        let link = self
            .host
            .invite(vec![self.addr.to_string()], None, now() + 600)
            .await
            .unwrap();
        Invite::from_link(&link, now()).unwrap()
    }

    /// Starts a joiner in the background and answers its knock with `admit`.
    /// Returns the joiner's node too: it must outlive the session.
    async fn knock(
        &mut self,
        invite: Invite,
        admit: bool,
    ) -> (CallNode, PublicId, Result<MemberHandle, CallError>) {
        let identity = Identity::generate();
        let device = identity.public();
        let joining = tokio::spawn(async move {
            let node = Node::bind(&identity, localhost()).unwrap();
            let node = CallNode::for_invite(&identity, node, &invite).await;
            let result = join(&node, &invite).await;
            (node, result)
        });
        loop {
            match within(self.events.recv()).await.unwrap() {
                HostEvent::Knock(knocker) => {
                    assert_eq!(knocker, device, "the knock names the pinned link key");
                    self.host.decide(device, admit).await.unwrap();
                    break;
                }
                HostEvent::Error(e) => panic!("host error: {e}"),
                _ => {}
            }
        }
        let (node, result) = within(joining).await.unwrap();
        (node, device, result)
    }

    async fn admit(&mut self) -> Member {
        let invite = self.invite().await;
        let (node, device, handle) = self.knock(invite, true).await;
        let mut handle = handle.unwrap();
        let keys = member_keys(&mut handle).await;
        self.keys = next_keys(&mut self.events).await;
        assert_eq!(keys.epoch, self.keys.epoch);
        Member { _node: node, device, handle, keys }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_members_share_each_epoch_with_the_host() {
    let mut call = start_call().await;
    let mut alice = call.admit().await;
    let mut bob = call.admit().await;
    assert_eq!(call.keys.epoch, 2);

    // Alice follows the host's commit for Bob's join.
    let mut alice_keys = member_keys(&mut alice.handle).await;
    assert_eq!(alice_keys.epoch, 2);
    let roster: Vec<PublicId> = call.keys.roster.iter().map(|(_, d)| *d).collect();
    assert!(roster.contains(&alice.device) && roster.contains(&bob.device));

    let packet = alice_keys.sender.encrypt(b"hello bob").unwrap();
    let (sender, plain) = bob.keys.receiver.decrypt(&packet).unwrap();
    assert_eq!(plain, b"hello bob");
    assert!(call.keys.roster.contains(&(sender, alice.device)), "sender index is Alice's leaf");
    let packet = call.keys.sender.encrypt(b"from host").unwrap();
    assert_eq!(alice_keys.receiver.decrypt(&packet).unwrap().1, b"from host");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declined_joiner_is_told_so() {
    let mut call = start_call().await;
    let invite = call.invite().await;
    let (_node, _, result) = call.knock(invite, false).await;
    assert!(matches!(result, Err(CallError::Refused(RefuseReason::DeclinedByHost))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_admits_one_device_only() {
    let mut call = start_call().await;
    let invite = call.invite().await;
    let (_node, _, first) = call.knock(invite.clone(), true).await;
    assert!(first.is_ok());
    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let node = CallNode::for_invite(&identity, node, &invite).await;
    let second = within(join(&node, &invite)).await;
    assert!(matches!(second, Err(CallError::Refused(RefuseReason::TokenUsed))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_link_is_a_leave_and_locks_the_leaver_out() {
    let mut call = start_call().await;
    let Member { device: alice, handle: alice_handle, keys: mut alice_keys, _node: alice_node } =
        call.admit().await;
    let mut bob = call.admit().await;
    drop(alice_handle);
    drop(alice_node);

    loop {
        match within(call.events.recv()).await.unwrap() {
            HostEvent::Left(device) => {
                assert_eq!(device, alice);
                break;
            }
            HostEvent::Error(e) => panic!("host error: {e}"),
            _ => {}
        }
    }
    let mut host_keys = next_keys(&mut call.events).await;
    assert_eq!(host_keys.epoch, 3);
    let mut bob_keys = member_keys(&mut bob.handle).await;
    assert_eq!(bob_keys.epoch, 3);

    let packet = host_keys.sender.encrypt(b"after alice").unwrap();
    assert_eq!(bob_keys.receiver.decrypt(&packet).unwrap().1, b"after alice");
    assert!(!host_keys.roster.iter().any(|(_, d)| *d == alice));
    // Alice's last keys were for epoch 1; she holds nothing for epoch 3.
    assert_eq!(alice_keys.receiver.decrypt(&packet), Err(SecurityError::UnknownKey));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_member_is_told_and_gets_no_more_keys() {
    let mut call = start_call().await;
    let mut alice = call.admit().await;
    call.host.remove(alice.device).await.unwrap();
    match within(alice.handle.events.recv()).await.unwrap() {
        MemberEvent::Removed => {}
        other => panic!("expected removal, got {other:?}"),
    }
    assert_eq!(next_keys(&mut call.events).await.epoch, 2);
}
