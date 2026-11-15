//! End-to-end group tests: lobby admission, epochs on join and leave, and media keys.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use rand_core_06::OsRng;
use veil_group::{Applied, Group, GroupError, Joiner, Lobby};
use veil_net::control::{Control, RefuseReason};
use veil_security::SecurityError;
use veil_security::invite::Invite;
use veil_security::{Identity, PublicId};

const NOW: u64 = 1_800_000_000;

struct Host {
    identity: Identity,
    group: Group,
    lobby: Lobby,
}

impl Host {
    fn new() -> Self {
        let identity = Identity::generate();
        let room_id = [9; 16];
        Self {
            group: Group::create(&identity, room_id).unwrap(),
            lobby: Lobby::new(room_id),
            identity,
        }
    }

    fn invite(&mut self) -> Invite {
        let mut invite = Invite::new(&self.identity, vec!["127.0.0.1:4433".into()], None, NOW + 60, &mut OsRng)
            .unwrap();
        invite.room_id = [9; 16];
        self.lobby.add_invite(&invite).unwrap();
        invite
    }

    fn hello(invite: &Invite) -> Control {
        Control::Hello {
            room_id: invite.room_id,
            join_token: invite.join_token,
        }
    }

    /// Runs the whole lobby flow and returns the joined member plus the commit for others.
    fn admit(&mut self, joiner: &Identity) -> (Group, Vec<u8>) {
        let invite = self.invite();
        let knock = self
            .lobby
            .knock(&Self::hello(&invite), joiner.public(), &self.group, NOW)
            .unwrap();
        assert_eq!(self.lobby.approve(knock).unwrap(), Control::Admitted);
        let pending = Joiner::new(joiner).unwrap();
        let package = Control::KeyPackage(pending.key_package().unwrap());
        let admission = self
            .lobby
            .admit(&mut self.group, &joiner.public(), &package)
            .unwrap();
        let member = pending
            .join(&admission.welcome, &self.identity.public())
            .unwrap();
        (member, admission.commit)
    }
}

#[test]
fn admitted_joiner_shares_media_keys_with_host() {
    let mut host = Host::new();
    let alice = Identity::generate();
    let (member, _) = host.admit(&alice);

    assert_eq!(member.epoch(), host.group.epoch());
    assert_eq!(member.epoch(), 1, "the join started a new epoch");
    assert_eq!((host.group.leaf_index(), member.leaf_index()), (0, 1));
    assert_eq!(*host.group.epoch_secret().unwrap(), *member.epoch_secret().unwrap());

    let mut host_tx = host.group.frame_sender().unwrap();
    let mut member_rx = member.frame_receiver().unwrap();
    let packet = host_tx.encrypt(b"host frame").unwrap();
    assert_eq!(member_rx.decrypt(&packet).unwrap(), (0, b"host frame".to_vec()));

    let mut member_tx = member.frame_sender().unwrap();
    let mut host_rx = host.group.frame_receiver().unwrap();
    let packet = member_tx.encrypt(b"alice frame").unwrap();
    assert_eq!(host_rx.decrypt(&packet).unwrap(), (1, b"alice frame".to_vec()));

    let roster = host.group.members();
    assert!(roster.contains(&(1, alice.public())));
}

#[test]
fn removed_member_cannot_decrypt_frames_from_the_next_epoch() {
    let mut host = Host::new();
    let alice = Identity::generate();
    let bob = Identity::generate();
    let (mut alice_group, _) = host.admit(&alice);
    let (mut bob_group, commit) = host.admit(&bob);
    assert_eq!(alice_group.apply(&commit).unwrap(), Applied::NewEpoch(2));

    // Alice keeps her receiver and secret from epoch 2.
    let mut alice_old_rx = alice_group.frame_receiver().unwrap();
    let alice_old_secret = alice_group.epoch_secret().unwrap();
    let old_epoch = alice_group.epoch();

    let commit = host.group.remove(&alice.public()).unwrap();
    assert_eq!(alice_group.apply(&commit).unwrap(), Applied::Removed);
    assert_eq!(bob_group.apply(&commit).unwrap(), Applied::NewEpoch(3));
    assert_eq!(host.group.epoch(), 3);
    assert!(!host.group.members().iter().any(|(_, d)| *d == alice.public()));

    // Alice can no longer export a secret.
    assert_eq!(alice_group.epoch_secret().map(|_| ()), Err(GroupError::Removed));
    assert!(alice_group.frame_receiver().is_err());

    let mut host_tx = host.group.frame_sender().unwrap();
    let packet = host_tx.encrypt(b"after removal").unwrap();

    // Bob, still a member, reads the new epoch.
    let mut bob_rx = bob_group.frame_receiver().unwrap();
    assert_eq!(bob_rx.decrypt(&packet).unwrap(), (0, b"after removal".to_vec()));

    // Alice's old receiver does not hold the new epoch's key.
    assert_eq!(alice_old_rx.decrypt(&packet), Err(SecurityError::UnknownKey));
    // Relabelling her old secret as the new epoch does not help either.
    let mut forged =
        veil_security::frame::FrameReceiver::new(&alice_old_secret, old_epoch + 1).unwrap();
    assert_eq!(forged.decrypt(&packet), Err(SecurityError::Decrypt));
}

#[test]
fn every_join_and_leave_starts_a_fresh_epoch() {
    let mut host = Host::new();
    let mut secrets = vec![*host.group.epoch_secret().unwrap()];
    let alice = Identity::generate();
    let (_alice_group, _) = host.admit(&alice);
    secrets.push(*host.group.epoch_secret().unwrap());
    host.group.remove(&alice.public()).unwrap();
    secrets.push(*host.group.epoch_secret().unwrap());
    assert_eq!(host.group.epoch(), 2);
    assert_ne!(secrets[0], secrets[1]);
    assert_ne!(secrets[1], secrets[2]);
    assert_ne!(secrets[0], secrets[2]);
}

#[test]
fn join_token_works_once() {
    let mut host = Host::new();
    let invite = host.invite();
    let hello = Host::hello(&invite);
    let first = Identity::generate().public();
    let second = Identity::generate().public();
    assert!(host.lobby.knock(&hello, first, &host.group, NOW).is_ok());
    assert_eq!(
        host.lobby.knock(&hello, second, &host.group, NOW),
        Err(GroupError::Refused(RefuseReason::TokenUsed))
    );
}

#[test]
fn unknown_room_unknown_token_and_expired_token_are_refused() {
    let mut host = Host::new();
    let device = Identity::generate().public();
    let wrong_room = Control::Hello { room_id: [1; 16], join_token: [2; 16] };
    assert_eq!(
        host.lobby.knock(&wrong_room, device, &host.group, NOW),
        Err(GroupError::Refused(RefuseReason::UnknownRoom))
    );
    let guessed = Control::Hello { room_id: [9; 16], join_token: [2; 16] };
    assert_eq!(
        host.lobby.knock(&guessed, device, &host.group, NOW),
        Err(GroupError::Refused(RefuseReason::TokenUsed))
    );
    let invite = host.invite();
    assert_eq!(
        host.lobby.knock(&Host::hello(&invite), device, &host.group, NOW + 60),
        Err(GroupError::Refused(RefuseReason::TokenUsed))
    );
    assert_eq!(
        host.lobby.knock(&Control::Admitted, device, &host.group, NOW),
        Err(GroupError::Unexpected("hello"))
    );
}

#[test]
fn declined_joiner_cannot_send_a_key_package() {
    let mut host = Host::new();
    let mallory = Identity::generate();
    let invite = host.invite();
    let knock = host
        .lobby
        .knock(&Host::hello(&invite), mallory.public(), &host.group, NOW)
        .unwrap();
    assert_eq!(
        host.lobby.decline(knock),
        Control::Refused(RefuseReason::DeclinedByHost)
    );
    let package = Control::KeyPackage(Joiner::new(&mallory).unwrap().key_package().unwrap());
    assert!(matches!(
        host.lobby.admit(&mut host.group, &mallory.public(), &package),
        Err(GroupError::NotMember)
    ));
    assert_eq!(host.group.epoch(), 0);
}

#[test]
fn key_package_from_another_device_is_refused() {
    let mut host = Host::new();
    let on_link = Identity::generate();
    let other = Identity::generate();
    let invite = host.invite();
    let knock = host
        .lobby
        .knock(&Host::hello(&invite), on_link.public(), &host.group, NOW)
        .unwrap();
    host.lobby.approve(knock).unwrap();
    let package = Control::KeyPackage(Joiner::new(&other).unwrap().key_package().unwrap());
    assert!(matches!(
        host.lobby.admit(&mut host.group, &on_link.public(), &package),
        Err(GroupError::WrongDevice)
    ));
}

#[test]
fn welcome_must_come_from_the_invited_host() {
    let mut host = Host::new();
    let alice = Identity::generate();
    let invite = host.invite();
    let knock = host
        .lobby
        .knock(&Host::hello(&invite), alice.public(), &host.group, NOW)
        .unwrap();
    host.lobby.approve(knock).unwrap();
    let pending = Joiner::new(&alice).unwrap();
    let package = Control::KeyPackage(pending.key_package().unwrap());
    let admission = host
        .lobby
        .admit(&mut host.group, &alice.public(), &package)
        .unwrap();
    let impostor = PublicId([5; 32]);
    assert!(matches!(
        pending.join(&admission.welcome, &impostor),
        Err(GroupError::WrongDevice)
    ));
}

#[test]
fn only_the_host_changes_the_group() {
    let mut host = Host::new();
    let alice = Identity::generate();
    let (mut alice_group, _) = host.admit(&alice);
    assert_eq!(
        alice_group.remove(&host.identity.public()).map(|_| ()),
        Err(GroupError::NotHost)
    );
    assert!(matches!(
        alice_group.add(&[], &Identity::generate().public()),
        Err(GroupError::NotHost)
    ));
}

#[test]
fn garbage_is_refused_without_panicking() {
    let mut host = Host::new();
    let alice = Identity::generate();
    let (mut alice_group, _) = host.admit(&alice);
    for bytes in [&[][..], &[0xff; 3][..], &[0; 64][..]] {
        assert!(alice_group.apply(bytes).is_err());
        assert!(host.group.add(bytes, &Identity::generate().public()).is_err());
        assert!(Joiner::new(&alice).unwrap().join(bytes, &host.identity.public()).is_err());
    }
}
