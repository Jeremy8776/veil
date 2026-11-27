//! A guest's part in a persistent call's handover (ADR 0010 §5, §7 and §9).
//!
//! Purpose: send the Available to host report, start host roles when offered, and follow the
//! new host, asking first when a relay-only member's relay would change operator.
//!
//! Invariants:
//! - Nothing is reported and no host role starts while Available to host is off.
//! - A relay-only member never dials the new host outside a relay, and with no answer to the
//!   relay question in the recovery window it leaves (the member session gives up).

use std::time::Instant;

use veil_call::{CallNode, MemberHandle, RelayAddrs};
use veil_security::{Identity, PublicId};

use crate::engine::availability::{REPORT_EVERY, Reach, report};
use crate::engine::host::successor::{Prepared, prepare};
use crate::engine::plan::NetConfig;
use crate::engine::session::Session;
use crate::engine::view::Shared;

#[derive(Default)]
pub(super) struct Handover {
    available: bool,
    reach: Reach,
    last_report: Option<Instant>,
    /// Host roles started for an offer, until the commit names this device.
    pub prepared: Option<Prepared>,
    /// The new host and its relay, while the person is asked about the relay change.
    asking: Option<(PublicId, Option<RelayAddrs>)>,
}

impl Handover {
    pub fn new(config: &NetConfig) -> Self {
        Self {
            reach: Reach {
                relay_only: config.relay_only,
                public: !config.advertise.is_empty()
                    || config.open_port
                    || (!config.local_only && veil_net::family::global_ipv6().is_some()),
            },
            ..Self::default()
        }
    }

    async fn send(&mut self, member: &MemberHandle, shared: &Shared) {
        let links = shared.stats().links;
        let _ = member
            .report_availability(report(self.available, self.reach, &links))
            .await;
        self.last_report = Some(Instant::now());
    }

    /// The Available to host switch changed.
    pub async fn set_available(&mut self, on: bool, member: &MemberHandle, shared: &Shared) {
        let on = on && !self.reach.relay_only;
        if on != self.available {
            self.available = on;
            self.send(member, shared).await;
        }
    }

    /// Refreshes an available member's report, at most every [`REPORT_EVERY`].
    pub async fn tick(&mut self, member: &MemberHandle, shared: &Shared) {
        let due = self
            .last_report
            .is_none_or(|at| at.elapsed() >= REPORT_EVERY);
        if self.available && due {
            self.send(member, shared).await;
        }
    }

    /// The host asks this device to take over: start host roles, or decline.
    #[allow(clippy::too_many_arguments)]
    pub async fn offered(
        &mut self,
        identity: &Identity,
        config: &NetConfig,
        call: &CallNode,
        room_id: [u8; 16],
        request: u64,
        epoch: u64,
        member: &MemberHandle,
    ) {
        let contact = if self.available {
            match prepare(identity, config, call, room_id, epoch).await {
                Ok(prepared) => {
                    let contact = prepared.contact.clone();
                    self.prepared = Some(prepared);
                    Some(contact)
                }
                Err(why) => {
                    crate::note!("call: could not start host roles: {why}");
                    None
                }
            }
        } else {
            None
        };
        let _ = member.handover_ready(request, contact).await;
    }

    /// The host handed over to `host`. Follows at once, or asks first (`ask`).
    #[allow(clippy::too_many_arguments)]
    pub async fn changing(
        &mut self,
        host: PublicId,
        relay: Option<RelayAddrs>,
        ask: bool,
        call: &CallNode,
        member: &MemberHandle,
        session: &mut Session,
        shared: &Shared,
    ) {
        if ask {
            let name = session.name_for(&host);
            shared.update(|view| view.handover.relay_ask = Some(name));
            self.asking = Some((host, relay));
            return;
        }
        follow(host, relay, call, member, session).await;
    }

    /// The person answered the relay question. `false` leaves the call.
    pub async fn relay_answer(
        &mut self,
        stay: bool,
        call: &CallNode,
        member: &MemberHandle,
        session: &mut Session,
        shared: &Shared,
    ) -> bool {
        shared.update(|view| view.handover.relay_ask = None);
        let Some((host, relay)) = self.asking.take() else {
            return true;
        };
        if stay {
            follow(host, relay, call, member, session).await;
        }
        stay
    }
}

async fn follow(
    host: PublicId,
    relay: Option<RelayAddrs>,
    call: &CallNode,
    member: &MemberHandle,
    session: &mut Session,
) {
    call.follow_host(host, relay).await;
    session.set_host(host);
    let _ = member.follow(call).await;
}
