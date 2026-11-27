//! Host-side control recovery: a member whose link drops stays a member for a bounded window
//! and can resume with its pinned key, receiving the commits it missed.
//!
//! Invariants:
//! - Resume is authorised by the pinned transport key and the CURRENT MLS roster, never by an
//!   invite. Removal, an expired window or a gap past the journal refuses it.
//! - A device cut off for falling behind is removed, never offered recovery.
//! - Replay is bounded by [`REPLAY_COMMITS`] and the link's outbox.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use veil_net::control::{Control, RefuseReason};
use veil_security::PublicId;

use super::{Actor, OUTBOX};

/// A disconnected device remains a member for this bounded window. Explicit leave and
/// removal bypass it. Replay is bounded in both count and bytes by the control frame limit.
pub const CONTROL_RECOVERY: Duration = Duration::from_secs(20);
pub(super) const REPLAY_COMMITS: usize = 64;

impl Actor {
    pub(super) fn remove_cut_off(&mut self) {
        // `remove` writes commits and may cut off more devices; each is removed once.
        let mut done = Vec::new();
        while let Some(device) = self.cut_off.pop() {
            if !done.contains(&device) {
                done.push(device);
                self.remove(device, false);
            }
        }
    }

    pub(super) fn expire_recovery(&mut self) {
        let now = Instant::now();
        let expired: Vec<_> = self
            .recovering
            .iter()
            .filter(|(_, (_, until))| *until <= now)
            .map(|(device, _)| *device)
            .collect();
        for device in expired {
            self.remove(device, false);
        }
    }

    /// A reconnect is authorized by the pinned transport identity and CURRENT MLS roster,
    /// never by an old invitation. Old link events cannot remove its replacement.
    pub(super) fn resume(
        &mut self,
        device: PublicId,
        link: u64,
        outbox: mpsc::Sender<Control>,
        room: [u8; 16],
        epoch: u64,
    ) {
        if self.handover.stepped_aside() {
            let _ = outbox.try_send(Control::Refused(RefuseReason::HostChanged));
            return;
        }
        self.expire_recovery();
        let current = self.group.epoch();
        let permitted = !self.cut_off.contains(&device)
            && room == self.lobby.room_id()
            && self.group.members().iter().any(|(_, id)| *id == device)
            && epoch <= current
            && (epoch == current
                || self
                    .history
                    .front()
                    .is_some_and(|(first, _)| *first <= epoch.saturating_add(1)));
        if !permitted {
            let _ = outbox.try_send(Control::Refused(RefuseReason::DeclinedByHost));
            return;
        }
        // Reserve enough room for the acknowledgment, every missed commit and the plan.
        let replay: Vec<_> = self
            .history
            .iter()
            .filter(|(at, _)| *at > epoch)
            .map(|(_, bytes)| Control::Commit(bytes.clone()))
            .collect();
        if replay.len() + 3 > OUTBOX {
            let _ = outbox.try_send(Control::Refused(RefuseReason::DeclinedByHost));
            return;
        }
        // A live link is replaced rather than refused: the member usually notices a dead path
        // before the host does. Only the device's own key can open this link, and dropping the
        // old outbox closes the old link.
        self.links.insert(device, (link, outbox));
        self.recovering.remove(&device);
        self.send_to(&device, Control::Resumed { epoch: current });
        for commit in replay {
            self.send_to(&device, commit);
        }
        if !self.addresses.is_empty() {
            self.send_to(&device, Control::Addresses(self.addresses.clone()));
        }
        if let Some((at, plan)) = &self.latest_plan
            && *at == current
        {
            self.send_to(&device, Control::Plan(plan.clone()));
        }
    }

    /// True when `link` is an old connection of this device.
    pub(super) fn stale(&self, device: &PublicId, link: u64) -> bool {
        self.links
            .get(device)
            .map(|(id, _)| *id)
            .or_else(|| self.recovering.get(device).map(|(id, _)| *id))
            .is_some_and(|current| current != link)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::hints::{HINT_GAP, member_hints};
    use super::super::test_support::{actor_for, add};
    use super::super::*;
    use super::*;
    use veil_group::Applied;

    fn actor() -> Actor {
        let identity = Identity::generate();
        let group = Group::create(&identity, [51; 16]).unwrap();
        actor_for(identity, group)
    }

    #[tokio::test]
    async fn a_recovering_member_applies_every_missed_commit_in_order() {
        let mut host = actor();
        let alice = Identity::generate();
        let mut group = add(&mut host, &alice);
        let before = group.epoch();
        let bob = Identity::generate();
        let _bob = add(&mut host, &bob);
        host.remove(bob.public(), false);
        assert_eq!(host.group.epoch(), before + 2);
        host.recovering
            .insert(alice.public(), (1, Instant::now() + CONTROL_RECOVERY));
        let (out, mut input) = mpsc::channel(OUTBOX);
        host.resume(alice.public(), 2, out, [51; 16], before);
        assert_eq!(
            input.recv().await.unwrap(),
            Control::Resumed { epoch: before + 2 }
        );
        for _ in 0..2 {
            let Control::Commit(commit) = input.recv().await.unwrap() else {
                panic!("missing commit")
            };
            assert!(matches!(
                group.apply(&commit).unwrap(),
                Applied::NewEpoch(_)
            ));
        }
        assert_eq!(group.epoch(), host.group.epoch());
        let sealed = host
            .group
            .frame_sender()
            .unwrap()
            .encrypt(b"caught up")
            .unwrap();
        assert_eq!(
            group.frame_receiver().unwrap().decrypt(&sealed).unwrap().1,
            b"caught up"
        );
        assert!(!host.recovering.contains_key(&alice.public()));
        // A delayed closure from the original connection cannot remove the resumed one.
        host.internal(Internal::Closed(alice.public(), 1, false));
        assert!(
            host.group
                .members()
                .iter()
                .any(|(_, id)| *id == alice.public())
        );
        assert!(!host.recovering.contains_key(&alice.public()));
    }

    #[test]
    fn member_hints_cannot_aim_traffic_at_third_parties() {
        let seen: SocketAddr = "81.2.69.160:5000".parse().unwrap();
        let hints = member_hints(
            vec![
                "192.168.1.9:5000".parse().unwrap(),
                "[fd00::9]:5000".parse().unwrap(),
                "[fe80::9]:5000".parse().unwrap(),
                "81.2.69.160:5001".parse().unwrap(),
                "81.2.69.160:5000".parse().unwrap(),
                "8.8.8.8:53".parse().unwrap(),
                // Another port on the LAN is someone else's service, not this member.
                "192.168.1.1:53".parse().unwrap(),
                "192.168.1.20:8080".parse().unwrap(),
                "127.0.0.1:5000".parse().unwrap(),
                "203.0.113.5:5000".parse().unwrap(),
            ],
            seen,
        )
        .unwrap();
        assert_eq!(
            hints,
            vec![seen]
        );
        let many = vec!["192.168.1.9:5000".parse().unwrap(); MAX_HINTS + 1];
        assert!(member_hints(many, seen).is_none());
        // A member on a privileged port gets no local hints, only its observed address.
        let low: SocketAddr = "192.168.1.9:53".parse().unwrap();
        assert_eq!(
            member_hints(vec!["192.168.1.30:53".parse().unwrap(), low], low).unwrap(),
            vec![low]
        );
    }

    #[tokio::test]
    async fn address_updates_are_throttled_and_the_newest_is_kept() {
        let (events, mut rx) = EventSender::channel();
        let mut host = actor();
        host.events = events;
        let device = PublicId([7; 32]);
        let one: Vec<SocketAddr> = vec!["192.168.1.2:1".parse().unwrap()];
        let two: Vec<SocketAddr> = vec!["192.168.1.3:1".parse().unwrap()];
        let three: Vec<SocketAddr> = vec!["192.168.1.4:1".parse().unwrap()];
        host.hint(device, one.clone());
        host.hint(device, two);
        host.hint(device, three.clone());
        assert!(matches!(rx.try_recv(), Ok(HostEvent::Addresses { addrs, .. }) if addrs == one));
        assert!(rx.try_recv().is_err(), "a burst produces one event");
        host.flush_hints();
        assert!(rx.try_recv().is_err(), "the gap has not passed");
        if let Some((last, _)) = host.hints.get_mut(&device) {
            *last = last.checked_sub(HINT_GAP).unwrap();
        }
        host.flush_hints();
        assert!(matches!(rx.try_recv(), Ok(HostEvent::Addresses { addrs, .. }) if addrs == three));
        host.remove(device, false);
        assert!(!host.hints.contains_key(&device));
    }

    #[tokio::test]
    async fn only_one_answer_per_asked_member_reaches_the_engine() {
        let (events, mut rx) = EventSender::channel();
        let mut host = actor();
        host.events = events;
        let asked = PublicId([3; 32]);
        host.checking = Some((9, std::collections::HashSet::from([asked])));
        host.message(PublicId([4; 32]), Control::PlanChecked { request: 9, ok: true });
        host.message(asked, Control::PlanChecked { request: 8, ok: true });
        assert!(rx.try_recv().is_err(), "strangers and old requests are dropped");
        host.message(asked, Control::PlanChecked { request: 9, ok: true });
        host.message(asked, Control::PlanChecked { request: 9, ok: false });
        assert!(matches!(rx.try_recv(), Ok(HostEvent::PlanChecked { ok: true, .. })));
        assert!(rx.try_recv().is_err(), "a second answer is dropped");
    }

    #[tokio::test]
    async fn a_member_cut_off_for_falling_behind_is_removed_not_resumed() {
        let mut host = actor();
        let alice = Identity::generate();
        let group = add(&mut host, &alice);
        let (out, _input) = mpsc::channel(1);
        host.links.insert(alice.public(), (1, out));
        host.send_to(&alice.public(), Control::Ping(1));
        host.send_to(&alice.public(), Control::Ping(2));
        assert!(host.cut_off.contains(&alice.public()));
        let (out, mut input) = mpsc::channel(OUTBOX);
        host.resume(alice.public(), 2, out, [51; 16], group.epoch());
        assert_eq!(
            input.recv().await.unwrap(),
            Control::Refused(RefuseReason::DeclinedByHost)
        );
        host.remove_cut_off();
        assert!(
            !host
                .group
                .members()
                .iter()
                .any(|(_, id)| *id == alice.public())
        );
    }

    #[tokio::test]
    async fn removal_during_an_outage_denies_resume_and_replay() {
        let mut host = actor();
        let alice = Identity::generate();
        let group = add(&mut host, &alice);
        host.recovering
            .insert(alice.public(), (1, Instant::now() + CONTROL_RECOVERY));
        host.remove(alice.public(), true);
        let (out, mut input) = mpsc::channel(OUTBOX);
        host.resume(alice.public(), 2, out, [51; 16], group.epoch());
        assert_eq!(
            input.recv().await.unwrap(),
            Control::Refused(RefuseReason::DeclinedByHost)
        );
        assert!(
            input.recv().await.is_none(),
            "no key updates are replayed to a removed identity"
        );
        assert!(!host.links.contains_key(&alice.public()));
    }

    #[tokio::test]
    async fn an_expired_recovery_window_revokes_membership_before_resume() {
        let mut host = actor();
        let alice = Identity::generate();
        let group = add(&mut host, &alice);
        host.recovering
            .insert(alice.public(), (1, Instant::now() - Duration::from_secs(1)));
        let (out, mut input) = mpsc::channel(OUTBOX);
        host.resume(alice.public(), 2, out, [51; 16], group.epoch());
        assert_eq!(
            input.recv().await.unwrap(),
            Control::Refused(RefuseReason::DeclinedByHost)
        );
        assert!(
            !host
                .group
                .members()
                .iter()
                .any(|(_, id)| *id == alice.public())
        );
    }

    #[tokio::test]
    async fn resume_refuses_wrong_rooms_future_epochs_and_unknown_keys() {
        let mut host = actor();
        let alice = Identity::generate();
        let group = add(&mut host, &alice);
        for (device, room, epoch) in [
            (alice.public(), [52; 16], group.epoch()),
            (alice.public(), [51; 16], group.epoch() + 1),
            (Identity::generate().public(), [51; 16], group.epoch()),
        ] {
            let (out, mut input) = mpsc::channel(OUTBOX);
            host.resume(device, 2, out, room, epoch);
            assert_eq!(input.recv().await.unwrap(), Control::Refused(RefuseReason::DeclinedByHost));
            assert!(input.recv().await.is_none(), "refusals disclose no replay or plan");
        }
    }

    #[tokio::test]
    async fn resume_refuses_an_epoch_older_than_the_available_journal() {
        let mut host = actor();
        let alice = Identity::generate();
        let group = add(&mut host, &alice);
        let bob = Identity::generate();
        let _bob = add(&mut host, &bob);
        host.remove(bob.public(), false);
        // Simulate the oldest missing commit having aged out of the bounded journal.
        host.history.retain(|(epoch, _)| *epoch > group.epoch() + 1);
        let (out, mut input) = mpsc::channel(OUTBOX);
        host.resume(alice.public(), 2, out, [51; 16], group.epoch());
        assert_eq!(input.recv().await.unwrap(), Control::Refused(RefuseReason::DeclinedByHost));
        assert!(input.recv().await.is_none());
    }
}
