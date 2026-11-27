//! Host actors for unit tests, with no links or app attached.
#![allow(clippy::unwrap_used)]

use tokio::sync::{mpsc, watch};
use veil_group::{Group, Joiner};
use veil_security::Identity;

use super::{Actor, Allowed};
use crate::events::EventSender;

/// A host actor for `group`, whose events go nowhere.
pub(super) fn actor_for(identity: Identity, group: Group) -> Actor {
    actor_in(identity, group, [51; 16])
}

pub(super) fn actor_in(identity: Identity, group: Group, room_id: [u8; 16]) -> Actor {
    let (events, _) = EventSender::channel();
    let (internal, _) = mpsc::channel(16);
    let (allowed, _) = watch::channel(Allowed::default());
    Actor::new(identity, group, room_id, events, internal, allowed)
}

/// Adds `identity` to the host's group and returns its member group. The commit goes into
/// the host's journal; nobody is sent it.
pub(super) fn add(actor: &mut Actor, identity: &Identity) -> Group {
    let joiner = Joiner::new(identity).unwrap();
    let admitted = actor
        .group
        .add(&joiner.key_package().unwrap(), &identity.public())
        .unwrap();
    actor.broadcast_commit(&admitted.commit, &[]);
    joiner
        .join(&admitted.welcome, &actor.identity.public())
        .unwrap()
}
