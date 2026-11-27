//! Tests for the leaving host's handover (`handover.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::super::test_support::{actor_for, add};
use super::*;
use crate::host::OUTBOX;
use tokio::sync::mpsc;
use veil_group::Group;
use veil_net::control::Reachable;
use veil_security::Identity;

const ROOM: [u8; 16] = [51; 16];

fn report(links: u8) -> Availability {
    Availability {
        available: true,
        reachable: Reachable::Good,
        direct_links: links,
        rtt_10ms: 3,
        upload_500k: 4,
    }
}

struct Member {
    id: Identity,
    group: Group,
    inbox: mpsc::Receiver<Control>,
}

fn persistent_call(n: usize) -> (Actor, Vec<Member>) {
    let identity = Identity::generate();
    let group = Group::create_persistent(&identity, ROOM).unwrap();
    let mut host = actor_for(identity, group);
    let mut members = Vec::new();
    for _ in 0..n {
        let id = Identity::generate();
        let group = add(&mut host, &id);
        let (out, inbox) = mpsc::channel(OUTBOX);
        host.links.insert(id.public(), (1, out));
        members.push(Member { id, group, inbox });
    }
    // Earlier members catch up on the commits that added later ones.
    let history: Vec<_> = host.history.iter().cloned().collect();
    for member in &mut members {
        for (epoch, commit) in &history {
            if *epoch > member.group.epoch() {
                member.group.apply(commit).unwrap();
            }
        }
    }
    (host, members)
}

fn contact(id: &Identity, epoch: u64) -> Vec<u8> {
    Contact {
        room_id: ROOM,
        epoch,
        host: id.public(),
        addrs: vec!["81.2.69.160:5000".parse().unwrap()],
        relay: None,
    }
    .seal(id)
    .unwrap()
}

fn drain(inbox: &mut mpsc::Receiver<Control>) -> Vec<Control> {
    let mut out = Vec::new();
    while let Ok(message) = inbox.try_recv() {
        out.push(message);
    }
    out
}

fn offered(inbox: &mut mpsc::Receiver<Control>) -> Option<u64> {
    drain(inbox).into_iter().find_map(|m| match m {
        Control::HandoverOffer { request, .. } => Some(request),
        _ => None,
    })
}

#[tokio::test]
async fn full_handover_with_three_members() {
    let (mut host, mut members) = persistent_call(3);
    host.availability(members[0].id.public(), report(1));
    host.availability(members[1].id.public(), report(2));
    let choices = host.choices();
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0].device, members[1].id.public(), "more direct links wins");

    let (reply, mut answer) = oneshot::channel();
    host.start_handover(None, reply);
    let request = offered(&mut members[1].inbox).expect("best member offered");
    let epoch = host.group.epoch();
    host.ready(members[1].id.public(), request, Some(contact(&members[1].id, epoch + 1)));
    assert_eq!(answer.try_recv().unwrap(), Ok(members[1].id.public()));
    assert!(!host.group.is_host());
    assert!(host.handover.stepped_aside());

    let successor = members[1].id.public();
    for member in &mut members {
        let messages = drain(&mut member.inbox);
        let Some(Control::Commit(commit)) = messages.first() else {
            panic!("commit first");
        };
        member.group.apply(commit).unwrap();
        let Some(Control::HandedOver { epoch: at, contact: sealed, .. }) = messages.get(1)
        else {
            panic!("handed over after the commit");
        };
        assert_eq!(member.group.host(), Some(successor));
        assert!(Contact::open(sealed, &successor, ROOM, *at).is_ok());
    }
    // Only the successor's link stays open on the old host.
    assert_eq!(host.links.len(), 1);
    assert!(host.links.contains_key(&successor));
}

#[tokio::test]
async fn the_host_s_pick_goes_first_and_a_failed_successor_is_skipped() {
    let (mut host, mut members) = persistent_call(3);
    for m in &members {
        host.availability(m.id.public(), report(1));
    }
    let pick = members[2].id.public();
    let (reply, mut answer) = oneshot::channel();
    host.start_handover(Some(pick), reply);
    let request = offered(&mut members[2].inbox).expect("pick offered first");
    // The pick declines; the next in automatic order is offered.
    host.ready(pick, request, None);
    let next = members
        .iter_mut()
        .take(2)
        .find_map(|m| offered(&mut m.inbox).map(|r| (m.id.public(), r)))
        .expect("next offered");
    // A contact signed by someone else is a failure too.
    host.ready(next.0, next.1, Some(contact(&members[2].id, host.group.epoch() + 1)));
    let last = members
        .iter_mut()
        .take(2)
        .find_map(|m| offered(&mut m.inbox).map(|r| (m.id.public(), r)))
        .expect("last offered");
    assert_ne!(last.0, next.0);
    let sealed = members
        .iter()
        .find(|m| m.id.public() == last.0)
        .map(|m| contact(&m.id, host.group.epoch() + 1))
        .unwrap();
    host.ready(last.0, last.1, Some(sealed));
    assert_eq!(answer.try_recv().unwrap(), Ok(last.0));
}

#[tokio::test]
async fn three_failures_leave_the_caller_host() {
    let (mut host, mut members) = persistent_call(4);
    for m in &members {
        host.availability(m.id.public(), report(1));
    }
    let epoch = host.group.epoch();
    let (reply, mut answer) = oneshot::channel();
    host.start_handover(None, reply);
    for _ in 0..MAX_ATTEMPTS {
        let (device, request) = members
            .iter_mut()
            .find_map(|m| offered(&mut m.inbox).map(|r| (m.id.public(), r)))
            .expect("an offer");
        host.ready(device, request, None);
    }
    assert_eq!(answer.try_recv().unwrap(), Err(HandoverFailure::NoneReady));
    assert!(host.group.is_host());
    assert_eq!(host.group.epoch(), epoch, "nothing was committed");
}

#[tokio::test]
async fn nobody_available_and_plain_calls_are_refused() {
    let (mut host, _members) = persistent_call(2);
    let (reply, mut answer) = oneshot::channel();
    host.start_handover(None, reply);
    assert_eq!(answer.try_recv().unwrap(), Err(HandoverFailure::NoOneAvailable));

    let identity = Identity::generate();
    let group = Group::create(&identity, ROOM).unwrap();
    let mut plain = actor_for(identity, group);
    let (reply, mut answer) = oneshot::channel();
    plain.start_handover(None, reply);
    assert_eq!(answer.try_recv().unwrap(), Err(HandoverFailure::NotPersistent));
}

#[tokio::test]
async fn relayed_and_unavailable_members_are_never_offered() {
    let (mut host, members) = persistent_call(2);
    host.handover.note_path(members[0].id.public(), true);
    host.availability(members[0].id.public(), report(5));
    let mut off = report(5);
    off.available = false;
    host.availability(members[1].id.public(), off);
    // A stranger's report is ignored.
    host.availability(Identity::generate().public(), report(9));
    assert!(host.choices().is_empty());
}

#[tokio::test]
async fn a_successor_that_does_not_answer_in_time_is_skipped() {
    let (mut host, mut members) = persistent_call(2);
    for m in &members {
        host.availability(m.id.public(), report(1));
    }
    let (reply, _answer) = oneshot::channel();
    host.start_handover(None, reply);
    let first = members
        .iter_mut()
        .position(|m| offered(&mut m.inbox).is_some())
        .unwrap();
    if let Stage::Offering { until, .. } = &mut host.handover.stage {
        *until = Instant::now() - Duration::from_millis(1);
    }
    host.handover_tick();
    assert!(offered(&mut members[1 - first].inbox).is_some());
}

#[tokio::test]
async fn after_the_commit_joiners_get_host_changed_and_the_removal_ends_it() {
    let (mut host, mut members) = persistent_call(2);
    host.availability(members[0].id.public(), report(1));
    // A joiner knocks and waits in the lobby.
    let invite = veil_security::invite::Invite::new(
        &host.identity,
        vec![],
        None,
        u64::MAX,
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
    )
    .unwrap();
    let mut invite = invite;
    invite.room_id = ROOM;
    host.lobby.add_invite(&invite).unwrap();
    let joiner = Identity::generate();
    let (out, mut joiner_inbox) = mpsc::channel(OUTBOX);
    host.links.insert(joiner.public(), (9, out));
    host.message(
        joiner.public(),
        Control::Hello {
            room_id: ROOM,
            join_token: invite.join_token,
        },
    );

    let (reply, _answer) = oneshot::channel();
    host.start_handover(None, reply);
    let request = offered(&mut members[0].inbox).unwrap();
    let next_epoch = host.group.epoch() + 1;
    host.ready(members[0].id.public(), request, Some(contact(&members[0].id, next_epoch)));
    assert_eq!(
        drain(&mut joiner_inbox).last(),
        Some(&Control::Refused(RefuseReason::HostChanged))
    );

    // A late joiner is refused too.
    let late = Identity::generate();
    let (out, mut late_inbox) = mpsc::channel(OUTBOX);
    host.links.insert(late.public(), (10, out));
    host.message(
        late.public(),
        Control::Hello {
            room_id: ROOM,
            join_token: [0; 16],
        },
    );
    assert_eq!(
        late_inbox.try_recv().unwrap(),
        Control::Refused(RefuseReason::HostChanged)
    );

    // The successor applies the handover and removes the old host.
    let successor = &mut members[0];
    for message in drain(&mut successor.inbox) {
        if let Control::Commit(commit) = message {
            successor.group.apply(&commit).unwrap();
        }
    }
    assert!(successor.group.is_host());
    let removal = successor.group.remove(&host.identity.public()).unwrap();
    // Only the successor's commit counts.
    host.successor_commit(members[1].id.public(), &removal);
    assert_eq!(host.handover.finished(), None);
    host.successor_commit(members[0].id.public(), &removal);
    assert_eq!(host.handover.finished(), Some(members[0].id.public()));
}

#[tokio::test]
async fn the_old_host_closes_after_the_linger_without_a_removal() {
    let (mut host, mut members) = persistent_call(1);
    host.availability(members[0].id.public(), report(1));
    let (reply, _answer) = oneshot::channel();
    host.start_handover(None, reply);
    let request = offered(&mut members[0].inbox).unwrap();
    let next_epoch = host.group.epoch() + 1;
    host.ready(members[0].id.public(), request, Some(contact(&members[0].id, next_epoch)));
    host.handover_tick();
    assert_eq!(host.handover.finished(), None);
    if let Stage::Handed { until, .. } = &mut host.handover.stage {
        *until = Instant::now() - Duration::from_millis(1);
    }
    host.handover_tick();
    assert_eq!(host.handover.finished(), Some(members[0].id.public()));
}
