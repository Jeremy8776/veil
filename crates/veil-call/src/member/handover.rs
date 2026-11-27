//! A member's side of a planned handover (ADR 0010 §7 steps 6 and 7).
//!
//! Purpose: decide what a `HandedOver` message means for this member. The successor takes
//! over with its own group ([`Promotion`]); every other member follows the new host at the
//! contact it signed. The decision is a pure function so it can be tested on its own.
//!
//! Invariants:
//! - `HandedOver` counts only after this member merged the handover commit, so the group
//!   itself names the new host. The old host cannot name anyone the commit did not.
//! - The contact must verify under that host's key, for this room and this epoch.
//! - A relay-only member never follows a host it can reach only directly.

use std::sync::Arc;

use veil_group::Group;
use veil_net::Node;
use veil_security::PublicId;

use crate::host::contact::Contact;
use crate::presence::Presence;

/// What the app tells a member session during a handover.
pub(super) enum MemberCommand {
    /// The successor's answer to `HandoverOffer`: its sealed contact, or `None` to decline.
    Ready {
        request: u64,
        contact: Option<Vec<u8>>,
    },
    /// The node moved its presence to the new host's room; dial the new host now.
    Follow {
        node: Arc<Node>,
        presence: Option<Presence>,
        relay_only: bool,
    },
    /// A coarse report of this device's own connection, for the host only.
    Availability(veil_net::control::Availability),
}

/// Everything the successor needs to host the call it was a member of. Hand it to
/// [`crate::HostHandle::take_over`].
pub struct Promotion {
    pub(crate) group: Group,
    pub(crate) room_id: [u8; 16],
    /// The commit that removed the old host, so resuming members can catch up.
    pub(crate) journal: Vec<(u64, Vec<u8>)>,
    /// Devices the old host removed or declined, for this lobby to flag.
    pub(crate) removed: Vec<PublicId>,
    pub(crate) old_host: PublicId,
}

impl Promotion {
    pub fn old_host(&self) -> PublicId {
        self.old_host
    }

    pub fn room_id(&self) -> [u8; 16] {
        self.room_id
    }
}

impl std::fmt::Debug for Promotion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Promotion")
            .field("epoch", &self.group.epoch())
            .field("old_host", &self.old_host)
            .finish_non_exhaustive()
    }
}

/// What to do with a `HandedOver` from the link's host.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    /// Not a handover this member has merged: an old message, or the group is unchanged.
    Ignore,
    /// This device is the new host.
    TakeOver,
    /// Dial the new host at its contact.
    Follow(PublicId, Contact),
    /// The handover cannot be followed. The call is lost for this member.
    Lost,
}

pub(super) fn decide(
    group: &Group,
    link_host: PublicId,
    room_id: [u8; 16],
    me: PublicId,
    epoch: u64,
    sealed: &[u8],
    relay_only: bool,
) -> Next {
    let Some(new) = group.host() else {
        return Next::Lost;
    };
    if new == link_host || epoch != group.epoch() {
        return Next::Ignore;
    }
    let Ok(contact) = Contact::open(sealed, &new, room_id, epoch) else {
        return Next::Lost;
    };
    if new == me {
        return Next::TakeOver;
    }
    if relay_only && contact.relay_addrs().is_none() {
        return Next::Lost;
    }
    Next::Follow(new, contact)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use veil_group::Joiner;
    use veil_security::Identity;

    const ROOM: [u8; 16] = [8; 16];

    /// A host, a successor and a third member, with the handover committed and merged.
    fn handed() -> (Identity, Identity, Group, Group, u64) {
        let host = Identity::generate();
        let mut group = Group::create_persistent(&host, ROOM).unwrap();
        let mut members = Vec::new();
        let ids = [Identity::generate(), Identity::generate()];
        for id in &ids {
            let joiner = Joiner::new(id).unwrap();
            let admitted = group
                .add(&joiner.key_package().unwrap(), &id.public())
                .unwrap();
            for member in &mut members {
                let member: &mut Group = member;
                member.apply(&admitted.commit).unwrap();
            }
            members.push(joiner.join(&admitted.welcome, &host.public()).unwrap());
        }
        let [next, _] = ids;
        let commit = group.hand_over(&next.public()).unwrap();
        for member in &mut members {
            member.apply(&commit).unwrap();
        }
        let third = members.pop().unwrap();
        let successor = members.pop().unwrap();
        let epoch = third.epoch();
        (host, next, successor, third, epoch)
    }

    fn contact(id: &Identity, epoch: u64, relay: Option<String>) -> Vec<u8> {
        Contact {
            room_id: ROOM,
            epoch,
            host: id.public(),
            addrs: if relay.is_some() {
                vec![]
            } else {
                vec!["81.2.69.160:5000".parse().unwrap()]
            },
            relay,
        }
        .seal(id)
        .unwrap()
    }

    #[test]
    fn the_successor_takes_over_and_others_follow() {
        let (host, next, successor, third, epoch) = handed();
        let sealed = contact(&next, epoch, None);
        assert_eq!(
            decide(&successor, host.public(), ROOM, next.public(), epoch, &sealed, false),
            Next::TakeOver
        );
        let me = Identity::generate().public();
        assert!(matches!(
            decide(&third, host.public(), ROOM, me, epoch, &sealed, false),
            Next::Follow(h, _) if h == next.public()
        ));
    }

    #[test]
    fn a_contact_not_signed_by_the_named_host_is_lost() {
        let (host, _next, _successor, third, epoch) = handed();
        let forged = contact(&host, epoch, None);
        let me = Identity::generate().public();
        assert_eq!(
            decide(&third, host.public(), ROOM, me, epoch, &forged, false),
            Next::Lost
        );
    }

    #[test]
    fn an_old_or_unmerged_handover_is_ignored() {
        let (host, next, _successor, third, epoch) = handed();
        let sealed = contact(&next, epoch, None);
        let me = Identity::generate().public();
        assert_eq!(
            decide(&third, host.public(), ROOM, me, epoch - 1, &sealed, false),
            Next::Ignore
        );
        // From the new host itself it means nothing.
        assert_eq!(
            decide(&third, next.public(), ROOM, me, epoch, &sealed, false),
            Next::Ignore
        );
    }

    #[test]
    fn a_relay_only_member_never_follows_a_direct_only_host() {
        let (host, next, _successor, third, epoch) = handed();
        let me = Identity::generate().public();
        let direct = contact(&next, epoch, None);
        assert_eq!(
            decide(&third, host.public(), ROOM, me, epoch, &direct, true),
            Next::Lost
        );
        let relay = format!("{}@81.2.69.160:7000", hex(&[3; 32]));
        let relayed = contact(&next, epoch, Some(relay));
        assert!(matches!(
            decide(&third, host.public(), ROOM, me, epoch, &relayed, true),
            Next::Follow(..)
        ));
    }

    fn hex(bytes: &[u8]) -> String {
        veil_net::NodeAddr {
            key: PublicId(bytes.try_into().unwrap()),
            addr: "81.2.69.160:7000".parse().unwrap(),
        }
        .to_string()
        .split('@')
        .next()
        .unwrap()
        .to_string()
    }
}
