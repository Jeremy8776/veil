//! Planned host handover in a persistent call over real loopback links (ADR 0010).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::time::Duration;

use tokio::sync::mpsc;
use veil_call::host::contact::Contact;
use veil_call::{CallNode, HostEvent, HostHandle, MemberEvent, MemberHandle, join};
use veil_net::Node;
use veil_net::control::{Availability, Reachable};
use veil_security::{Identity, invite::Invite};

const ROOM: [u8; 16] = [44; 16];

async fn within<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), f)
        .await
        .expect("timed out")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Guest {
    identity: Identity,
    node: CallNode,
    member: MemberHandle,
}

async fn admit(handle: &HostHandle, events: &mut mpsc::Receiver<HostEvent>, addr: String) -> Guest {
    let link = handle.invite(vec![addr], None, now() + 600).await.unwrap();
    let invite = Invite::from_link(&link, now()).unwrap();
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
    let (node, member) = within(joining).await.unwrap();
    Guest {
        identity,
        node,
        member,
    }
}

/// The next event that is not a key change.
async fn next(member: &mut MemberHandle) -> MemberEvent {
    loop {
        match within(member.events.recv()).await.unwrap() {
            MemberEvent::Keys(_) => {}
            event => return event,
        }
    }
}

fn report(available: bool) -> Availability {
    Availability {
        available,
        reachable: Reachable::Good,
        direct_links: 1,
        rtt_10ms: 1,
        upload_500k: 4,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persistent_call_survives_its_host_leaving() {
    let identity = Identity::generate();
    let node = Node::bind(&identity, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = node.local_addr().unwrap().to_string();
    let (handle, mut events) = HostHandle::start_persistent(&identity, ROOM).unwrap();
    let _host_node = CallNode::start(&identity, node, ROOM, None, handle.clone()).await;
    let mut ana = admit(&handle, &mut events, addr.clone()).await;
    let mut bob = admit(&handle, &mut events, addr).await;

    ana.member.report_availability(report(true)).await.unwrap();
    bob.member.report_availability(report(false)).await.unwrap();
    within(async {
        while handle.choices().await.unwrap().len() != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        handle.choices().await.unwrap()[0].device,
        ana.identity.public()
    );

    let leaving = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.hand_over(None).await })
    };
    let MemberEvent::HandoverOffer { request, epoch } = next(&mut ana.member).await else {
        panic!("ana is offered hosting");
    };
    let contact = Contact {
        room_id: ROOM,
        epoch: epoch + 1,
        host: ana.identity.public(),
        addrs: vec![ana.node.local_addr().unwrap()],
        relay: None,
    }
    .seal(&ana.identity)
    .unwrap();
    ana.member.handover_ready(request, Some(contact)).await.unwrap();
    assert_eq!(
        within(leaving).await.unwrap(),
        Ok(ana.identity.public())
    );

    // Ana takes over with her own key and group state.
    let MemberEvent::Promoted(promotion) = next(&mut ana.member).await else {
        panic!("ana is promoted");
    };
    assert_eq!(promotion.old_host(), identity.public());
    let (ana_host, mut ana_events) = HostHandle::take_over(&ana.identity, *promotion).unwrap();
    ana.node.promote(ana_host.clone(), None).await;

    // Bob follows Ana and catches up on the commit that removed the old host.
    let MemberEvent::HostChanging { host, ask, .. } = next(&mut bob.member).await else {
        panic!("bob is told the host changed");
    };
    assert_eq!(host, ana.identity.public());
    assert!(!ask);
    bob.node.follow_host(host, None).await;
    bob.member.follow(&bob.node).await.unwrap();
    assert!(matches!(
        next(&mut bob.member).await,
        MemberEvent::HostChanged(h) if h == ana.identity.public()
    ));
    assert_eq!(bob.member.link().peer(), ana.identity.public());

    // The old host stepped aside without ending the call.
    loop {
        match within(events.recv()).await {
            Some(HostEvent::HandedOver { to }) => {
                assert_eq!(to, ana.identity.public());
                break;
            }
            Some(_) => {}
            None => panic!("the old host's session ended without handing over"),
        }
    }

    // Ana's roster is Ana and Bob, and she can change it.
    let roster = loop {
        if let HostEvent::Keys(keys) = within(ana_events.recv()).await.unwrap() {
            break keys.roster;
        }
    };
    let devices: Vec<_> = roster.into_iter().map(|(_, d)| d).collect();
    assert!(devices.contains(&bob.identity.public()));
    assert!(!devices.contains(&identity.public()));
    ana_host.remove(bob.identity.public()).await.unwrap();
    assert!(matches!(next(&mut bob.member).await, MemberEvent::Removed));
}
