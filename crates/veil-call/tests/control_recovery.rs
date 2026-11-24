//! Real control-connection recovery, without fresh invitations or a rendezvous.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::time::Duration;
use tokio::sync::mpsc;
use veil_call::{CallNode, HostEvent, HostHandle, MemberEvent, MemberHandle, join};
use veil_net::{
    Node,
    control::{Control, RefuseReason},
};
use veil_security::{Identity, invite::Invite};

async fn within<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

struct Call {
    _host: CallNode,
    handle: HostHandle,
    events: mpsc::Receiver<HostEvent>,
    guest: CallNode,
    member: MemberHandle,
    epoch: u64,
}

async fn call() -> Call {
    let identity = Identity::generate();
    let node = Node::bind(&identity, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = node.local_addr().unwrap();
    let (handle, mut events) = HostHandle::start(&identity, [31; 16]).unwrap();
    let host = CallNode::start(&identity, node, [31; 16], None, handle.clone()).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invite = handle
        .invite(vec![addr.to_string()], None, now + 600)
        .await
        .unwrap();
    let invite = Invite::from_link(&invite, now).unwrap();
    let identity = Identity::generate();
    let node = Node::bind(&identity, "127.0.0.1:0".parse().unwrap()).unwrap();
    let guest = CallNode::for_invite(&identity, node, &invite).await;
    let joining = tokio::spawn(async move {
        let member = join(&guest, &invite).await.unwrap();
        (guest, member)
    });
    loop {
        if let HostEvent::Knock(device) = within(events.recv()).await.unwrap() {
            handle.decide(device, true).await.unwrap();
            break;
        }
    }
    let (guest, mut member) = within(joining).await.unwrap();
    let epoch = match within(member.events.recv()).await.unwrap() {
        MemberEvent::Keys(keys) => keys.epoch,
        event => panic!("unexpected initial event: {event:?}"),
    };
    Call {
        _host: host,
        handle,
        events,
        guest,
        member,
        epoch,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transient_control_loss_resumes_the_same_membership() {
    let mut call = call().await;
    let old = call.member.link();
    old.close("simulated network interruption");
    assert!(matches!(
        within(call.member.events.recv()).await.unwrap(),
        MemberEvent::Reconnecting
    ));
    assert!(matches!(
        within(call.member.events.recv()).await.unwrap(),
        MemberEvent::Reconnected
    ));
    let new = call.member.link();
    assert!(!std::sync::Arc::ptr_eq(&old, &new));
    assert_eq!(new.peer(), call._host.id());
    // Recovery does not burn another invitation, rotate the epoch, or emit a departure.
    while let Ok(event) = call.events.try_recv() {
        match event {
            HostEvent::Left(_) => panic!("transient loss removed the member"),
            HostEvent::Keys(keys) => assert_eq!(keys.epoch, call.epoch),
            _ => {}
        }
    }
    call.handle.remove(call.guest.id()).await.unwrap();
    assert!(matches!(
        within(call.member.events.recv()).await.unwrap(),
        MemberEvent::Removed
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_identity_cannot_resume_with_old_membership_state() {
    let mut call = call().await;
    call.handle.remove(call.guest.id()).await.unwrap();
    assert!(matches!(
        within(call.member.events.recv()).await.unwrap(),
        MemberEvent::Removed
    ));
    let node = Node::bind(call.guest.identity(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let link = within(node.connect(&[call._host.local_addr().unwrap()], call._host.id()))
        .await
        .unwrap();
    let mut control = link.open_control().await.unwrap();
    control
        .send(&Control::Resume {
            room_id: call.guest.room_id(),
            epoch: call.epoch,
        })
        .await
        .unwrap();
    assert_eq!(
        within(control.recv()).await.unwrap(),
        Control::Refused(RefuseReason::DeclinedByHost)
    );
}
