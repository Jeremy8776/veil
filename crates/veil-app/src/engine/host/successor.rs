//! A member becoming host of a persistent call (ADR 0010 §7 steps 3 and 7).
//!
//! Purpose: on `HandoverOffer` the member starts its own host roles with its own settings,
//! exactly as `host::run` would: its configured `veil-node`, or a rendezvous and forwarder of
//! its own. It answers with a contact signed by its own key. On `Promoted` it builds the
//! host's loop ([`Hosting`]) from the call it is already in: the same node, media session
//! and people, now with its own `HostHandle`.
//!
//! Invariants:
//! - Nothing of the old host moves: no key, no invite, no rendezvous room, no relay.
//! - Host roles start only with "Available to host" on, which is the consent (§5).
//!
//! shortcut: helpers started for an offer that is then given to someone else keep running
//! until the call ends. They serve only this device's own room, and a later offer reuses them
//! ([`Prepared::reseal`]). Stop them on a lost offer if people hand over often.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::mpsc::Receiver;
use veil_call::host::contact::Contact;
use veil_call::{CallNode, HostEvent, HostHandle, Promotion, RelayAddrs};
use veil_net::NodeAddr;
use veil_security::Identity;

use super::hosting::Hosting;
use super::{Own, check, hosted_addresses, new_invite, own_server, publish_addresses};
use crate::engine::helper_evidence::HelperWatch;
use crate::engine::pinhole::V6Port;
use crate::engine::plan::{NetConfig, advertised, lan_ip, resolve};
use crate::engine::reach::Reachability;
use crate::engine::router::{PortStatus, Router};
use crate::engine::session::Session;
use crate::engine::view::Shared;

/// Host roles started for an offer, waiting for the handover commit.
pub(in crate::engine) struct Prepared {
    pub contact: Vec<u8>,
    rendezvous: NodeAddr,
    configured: Option<NodeAddr>,
    forwarder: Option<NodeAddr>,
    relay: RelayAddrs,
    addrs: Vec<SocketAddr>,
    port: u16,
}

/// Starts this member's host roles for the call `room_id`, whose handover would start
/// epoch `epoch + 1`, and seals the contact members will dial.
pub(in crate::engine) async fn prepare(
    identity: &Identity,
    config: &NetConfig,
    call: &CallNode,
    room_id: [u8; 16],
    epoch: u64,
) -> Result<Prepared, String> {
    let port = call.local_addr().map_err(|e| e.to_string())?.port();
    let lan = lan_ip();
    let v6 = if config.local_only {
        None
    } else {
        veil_net::family::global_ipv6()
    };
    let configured = resolve(config.rendezvous.as_ref(), "rendezvous").await?;
    let rendezvous = match configured {
        Some(addr) => addr,
        None => own_server(lan, Own::Rendezvous, identity, room_id, config.local_only)?,
    };
    let forwarder = match (resolve(config.forwarder.as_ref(), "forwarder").await?, configured) {
        (Some(addr), _) => Some(addr),
        (None, None) => Some(own_server(
            lan,
            Own::Forwarder,
            identity,
            room_id,
            config.local_only,
        )?),
        (None, Some(_)) => None,
    };
    let mut relay = RelayAddrs::from(rendezvous);
    if configured.is_none() {
        relay.addrs = hosted_addresses(rendezvous, None, config.local_only);
    }
    let addrs = advertised(config, v6, lan, port);
    let contact = seal(identity, room_id, epoch, &addrs, &relay)?;
    Ok(Prepared {
        contact,
        rendezvous,
        configured,
        forwarder,
        relay,
        addrs,
        port,
    })
}

impl Prepared {
    /// A later offer in the same call (an earlier one timed out or went elsewhere): the host
    /// roles already run, so only the contact is sealed again, for the new epoch. Starting
    /// them again would leave a second rendezvous and forwarder running.
    pub(in crate::engine) fn reseal(
        &mut self,
        identity: &Identity,
        room_id: [u8; 16],
        epoch: u64,
    ) -> Result<(), String> {
        self.contact = seal(identity, room_id, epoch, &self.addrs, &self.relay)?;
        Ok(())
    }
}

/// The contact for a handover that would start epoch `epoch + 1`.
fn seal(
    identity: &Identity,
    room_id: [u8; 16],
    epoch: u64,
    addrs: &[SocketAddr],
    relay: &RelayAddrs,
) -> Result<Vec<u8>, String> {
    Contact {
        room_id,
        epoch: epoch.saturating_add(1),
        host: identity.public(),
        addrs: addrs.to_vec(),
        relay: Some(relay.to_string()),
    }
    .seal(identity)
    .map_err(|e| e.to_string())
}

/// Hosts the call from `promotion` with the roles `prepared` started. Returns the host's
/// loop, ready to run.
pub(in crate::engine) async fn take_over(
    identity: &Identity,
    config: NetConfig,
    call: CallNode,
    mut session: Session,
    promotion: Promotion,
    prepared: Prepared,
    shared: Arc<Shared>,
) -> Result<Hosting, String> {
    let (host, events): (HostHandle, Receiver<HostEvent>) =
        HostHandle::take_over(identity, promotion).map_err(|e| e.to_string())?;
    call.promote(host.clone(), Some(prepared.relay.clone())).await;
    let me = identity.public();
    session.set_host(me);
    let Prepared {
        rendezvous,
        configured,
        forwarder,
        relay,
        addrs,
        port,
        ..
    } = prepared;
    publish_addresses(&host, &addrs).await;
    let lan = lan_ip();
    let router = Router::start(config.open_port, lan, port);
    let relay_router = match configured {
        None => Router::start(config.open_port, lan, rendezvous.addr.port()),
        Some(_) => None,
    };
    let ports = PortStatus::new(router.as_ref(), relay_router.as_ref());
    let v6 = if config.local_only {
        None
    } else {
        veil_net::family::global_ipv6()
    };
    let v6_port = V6Port::start(config.open_port, v6, port);
    let mut reach = Reachability::default();
    if let Some(at) = config.advertise.first() {
        check(&mut reach, &call, configured, *at);
    }
    let helper_watch = HelperWatch::start(&call, true, true);
    let expires = new_invite(
        &host,
        &call,
        &addrs,
        &relay,
        false,
        config.local_only,
        &shared,
    )
    .await;
    shared.update(|view| {
        view.hosting = true;
        view.handover.notice =
            Some("You are now the host. Earlier invite links no longer work.".into());
        view.router = ports.view();
    });
    Ok(Hosting {
        me,
        server_down: !call.has_rendezvous(),
        shared,
        host,
        events,
        call,
        session,
        rendezvous,
        configured,
        configured6: None,
        forwarder,
        helper_watch,
        port,
        addrs,
        relay,
        until_end: false,
        expires,
        router,
        relay_router,
        ports,
        v6_port,
        at6: None,
        reach,
        persistent: true,
        config,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_later_offer_reseals_the_same_roles_for_its_epoch() {
        let me = Identity::generate();
        let room = [4; 16];
        let node = NodeAddr {
            key: Identity::generate().public(),
            addr: "81.2.69.160:7000".parse().unwrap(),
        };
        let addrs: Vec<SocketAddr> = vec!["81.2.69.160:5000".parse().unwrap()];
        let relay = RelayAddrs::from(node);
        let mut prepared = Prepared {
            contact: seal(&me, room, 3, &addrs, &relay).unwrap(),
            rendezvous: node,
            configured: None,
            forwarder: None,
            relay: relay.clone(),
            addrs: addrs.clone(),
            port: 5000,
        };
        prepared.reseal(&me, room, 8).unwrap();
        let opened = Contact::open(&prepared.contact, &me.public(), room, 9).unwrap();
        assert_eq!(opened.addrs, addrs);
        assert_eq!(opened.relay_addrs(), Some(relay));
        assert!(Contact::open(&prepared.contact, &me.public(), room, 4).is_err());
    }
}
