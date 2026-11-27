//! Planned host handover in a persistent call (ADR 0010): authority moves by one commit,
//! the old host is fenced by the epoch, and nobody's private key moves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use veil_group::{Applied, CallAuthority, Group, GroupError, Joiner};
use veil_security::Identity;

struct Call {
    ids: Vec<Identity>,
    groups: Vec<Group>,
}

impl Call {
    /// A call of `n` devices. Device 0 created it and hosts.
    fn new(n: usize, persistent: bool) -> Self {
        let host = Identity::generate();
        let group = if persistent {
            Group::create_persistent(&host, [4; 16]).unwrap()
        } else {
            Group::create(&host, [4; 16]).unwrap()
        };
        let mut call = Self {
            ids: vec![host],
            groups: vec![group],
        };
        for _ in 1..n {
            call.join(Identity::generate());
        }
        call
    }

    fn host_index(&self) -> usize {
        self.groups.iter().position(Group::is_host).unwrap()
    }

    fn join(&mut self, id: Identity) {
        let host = self.host_index();
        let host_key = self.ids[host].public();
        let pending = Joiner::new(&id).unwrap();
        let admission = self.groups[host]
            .add(&pending.key_package().unwrap(), &id.public())
            .unwrap();
        self.broadcast(host, &admission.commit);
        let member = pending.join(&admission.welcome, &host_key).unwrap();
        self.ids.push(id);
        self.groups.push(member);
    }

    /// Every member except `from` applies `commit`.
    fn broadcast(&mut self, from: usize, commit: &[u8]) -> Vec<Applied> {
        let mut out = Vec::new();
        for (i, group) in self.groups.iter_mut().enumerate() {
            if i != from && group.is_active() {
                out.push(group.apply(commit).unwrap());
            }
        }
        out
    }

    fn assert_same_keys(&self) {
        let active: Vec<&Group> = self.groups.iter().filter(|g| g.is_active()).collect();
        let secret = active[0].epoch_secret().unwrap();
        for group in &active[1..] {
            assert_eq!(*group.epoch_secret().unwrap(), *secret);
        }
    }
}

#[test]
fn persistent_call_hands_over_and_new_host_removes_old_one() {
    let mut call = Call::new(4, true);
    let (old, next) = (0, 2);
    let next_key = call.ids[next].public();
    assert!(call.groups.iter().all(Group::is_persistent));

    let epoch = call.groups[old].epoch();
    let commit = call.groups[old].hand_over(&next_key).unwrap();
    call.broadcast(old, &commit);

    for group in &call.groups {
        assert_eq!(group.epoch(), epoch + 1);
        assert_eq!(group.host(), Some(next_key));
    }
    assert!(
        !call.groups[old].is_host(),
        "the old host is fenced as soon as it commits"
    );
    assert!(call.groups[next].is_host());
    assert_eq!(
        call.groups[old].authority().unwrap(),
        Some(CallAuthority {
            host: next_key,
            persistent: true
        })
    );
    call.assert_same_keys();

    // The old host can no longer change the group.
    let someone = call.ids[1].public();
    assert_eq!(call.groups[old].remove(&someone), Err(GroupError::NotHost));

    // The new host's first commit removes the old host.
    let old_key = call.ids[old].public();
    let removal = call.groups[next].remove(&old_key).unwrap();
    let applied = call.broadcast(next, &removal);
    assert!(applied.contains(&Applied::Removed));
    assert!(!call.groups[old].is_active());
    assert!(call.groups[old].epoch_secret().is_err());
    call.assert_same_keys();

    // The new host admits people, and joiners check the invite's host against the authority.
    call.join(Identity::generate());
    call.assert_same_keys();
    assert_eq!(call.groups.last().unwrap().host(), Some(next_key));
}

#[test]
fn a_member_cannot_hand_over_and_a_plain_call_cannot_either() {
    let mut call = Call::new(3, true);
    let target = call.ids[2].public();
    assert_eq!(call.groups[1].hand_over(&target), Err(GroupError::NotHost));

    let mut plain = Call::new(3, false);
    let target = plain.ids[1].public();
    assert_eq!(
        plain.groups[0].hand_over(&target),
        Err(GroupError::NotPersistent)
    );
    assert!(!plain.groups[0].is_persistent());
    assert_eq!(plain.groups[1].host(), Some(plain.ids[0].public()));
}

#[test]
fn handover_must_name_another_current_member() {
    let mut call = Call::new(2, true);
    let me = call.ids[0].public();
    let stranger = Identity::generate().public();
    assert_eq!(call.groups[0].hand_over(&me), Err(GroupError::NotMember));
    assert_eq!(
        call.groups[0].hand_over(&stranger),
        Err(GroupError::NotMember)
    );
    let epoch = call.groups[0].epoch();
    assert_eq!(
        call.groups[0].epoch(),
        epoch,
        "a refused handover starts no epoch"
    );
}

#[test]
fn a_removed_member_cannot_be_named_successor() {
    let mut call = Call::new(3, true);
    let gone = call.ids[1].public();
    let commit = call.groups[0].remove(&gone).unwrap();
    call.broadcast(0, &commit);
    assert_eq!(call.groups[0].hand_over(&gone), Err(GroupError::NotMember));
}

#[test]
fn an_ordinary_member_commit_is_still_refused_after_handover() {
    let mut call = Call::new(3, true);
    let next = call.ids[1].public();
    let commit = call.groups[0].hand_over(&next).unwrap();
    call.broadcast(0, &commit);
    // Device 2 is an ordinary member, exactly like before.
    let other = call.ids[0].public();
    assert_eq!(call.groups[2].remove(&other), Err(GroupError::NotHost));
}

/// A key package from a Veil that predates persistent calls: no authority capability.
fn old_version_key_package(id: &Identity) -> Vec<u8> {
    use openmls::prelude::{BasicCredential, CredentialWithKey, KeyPackage};
    use openmls_traits::signatures::{Signer, SignerError};
    use openmls_traits::types::SignatureScheme;
    use tls_codec::Serialize as _;

    struct Sign<'a>(&'a Identity);
    impl Signer for Sign<'_> {
        fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
            Ok(self.0.sign(payload).to_vec())
        }
        fn signature_scheme(&self) -> SignatureScheme {
            SignatureScheme::ED25519
        }
    }
    let public = id.public().0.to_vec();
    let credential = CredentialWithKey {
        credential: BasicCredential::new(public.clone()).into(),
        signature_key: public.into(),
    };
    let provider = veil_group::provider::VeilProvider::new().unwrap();
    KeyPackage::builder()
        .build(
            veil_group::provider::CIPHERSUITE,
            &provider,
            &Sign(id),
            credential,
        )
        .unwrap()
        .key_package()
        .tls_serialize_detached()
        .unwrap()
}

#[test]
fn an_older_veil_cannot_join_a_persistent_call_but_can_join_a_plain_one() {
    let old = Identity::generate();
    let mut persistent = Call::new(1, true);
    assert!(matches!(
        persistent.groups[0].add(&old_version_key_package(&old), &old.public()),
        Err(GroupError::Mls(_))
    ));
    let mut plain = Call::new(1, false);
    assert!(
        plain.groups[0]
            .add(&old_version_key_package(&old), &old.public())
            .is_ok()
    );
}
