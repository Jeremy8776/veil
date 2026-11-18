//! A participant's network presence for one call: its node, its router and its rendezvous.
//!
//! Purpose: every participant, host or joiner, starts a [`CallNode`]. It accepts links on the
//! node's socket and routes them (joiners to the host session, members to media), registers
//! with the call's rendezvous node if there is one, and dials peers (`dial.rs`: direct, punched
//! and relayed paths, started in stages).
//!
//! Invariants:
//! - Every dial names the key it expects, on every path. A relayed link is still the two
//!   participants' own pinned QUIC link.
//! - A rendezvous node that cannot be reached is not fatal: the call still works over direct
//!   addresses, and [`CallNode::has_rendezvous`] reports the loss.
//! - Background tasks stop when the node is dropped, or when starting it is cancelled.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{room_tag, store_invite};
use veil_security::invite::{Invite, lobby_pass};
use veil_security::short_invite::ShortInvite;
use veil_security::{Identity, PublicId};

use crate::dial::{Dialled, dial};
use crate::host::HostHandle;
use crate::inbound::{Inbound, Router};
use crate::presence::{Consent, Presence, Seat};
use crate::relay_field::RelayAddrs;
use crate::{CallError, media};

/// Incoming media links waiting for the media session.
const MEDIA_INBOX: usize = 32;
/// How long the host waits to leave a sealed invite on the rendezvous. The app's call loop
/// waits on this, so it stays short; a node this slow gets the full link instead.
const SHORTEN_LIMIT: Duration = Duration::from_secs(2);

/// How a link reached its peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// Dialled at an address the peer gave.
    Direct,
    /// Dialled at the same moment as the peer, after a rendezvous introduction.
    Punched,
    /// Carried inside a relay session on the rendezvous node.
    Relayed,
    /// To a forwarder, which carries every member's media.
    Forwarder,
}

pub struct CallNode {
    node: Arc<Node>,
    identity: Identity,
    room_id: [u8; 16],
    presence: Option<Presence>,
    relay_only: bool,
    consent: Arc<Consent>,
    media: Option<mpsc::Receiver<Inbound>>,
    // Held only so dropping the node aborts them.
    _tasks: Tasks,
}

/// The node's background tasks, aborted when dropped. Held from the first spawn, so a start
/// that is cancelled part-way (Leave while registering) stops them too, rather than leaving
/// them running with the socket open.
#[derive(Default)]
struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

impl CallNode {
    /// The host's node for the call `room_id`: joiners reach `host`, its session.
    /// `rendezvous` is the call's rendezvous and relay node, if any; the host opens its room
    /// there and keeps the room's allow list current.
    pub async fn start(
        identity: &Identity,
        node: Node,
        room_id: [u8; 16],
        rendezvous: Option<NodeAddr>,
        host: HostHandle,
    ) -> Self {
        let seat = Seat::Host(host.allowed());
        let rendezvous = rendezvous.map(RelayAddrs::from);
        Self::launch(identity, node, room_id, rendezvous, Some(host), seat).await
    }

    async fn launch(
        identity: &Identity,
        node: Node,
        room_id: [u8; 16],
        rendezvous: Option<RelayAddrs>,
        host: Option<HostHandle>,
        seat: Seat,
    ) -> Self {
        let node = Arc::new(node);
        let (media_tx, media_rx) = mpsc::channel(MEDIA_INBOX);
        let router = Router {
            room_id,
            host,
            media: media_tx,
        };
        let consent = Arc::new(Consent {
            open: router.host.is_some(),
            ..Consent::default()
        });
        let mut tasks = Tasks::default();
        tasks.0.push(tokio::spawn(router.clone().accept_loop(node.clone())));
        let mut presence = None;
        if let Some(server) = rendezvous
            && let Ok((handle, task)) = Presence::start(
                identity,
                node.clone(),
                &server,
                router,
                consent.clone(),
                seat,
            )
            .await
        {
            presence = Some(handle);
            tasks.0.push(task);
        }
        Self {
            node,
            identity: Identity::from_secret(&identity.secret_bytes()),
            room_id,
            presence,
            relay_only: false,
            consent,
            media: Some(media_rx),
            _tasks: tasks,
        }
    }

    /// Starts a joiner's node for the call an invite names, using the invite's relay field as
    /// the rendezvous node when it holds `key@ip:port`, perhaps with more addresses
    /// ([`RelayAddrs`]), all dialled at once.
    pub async fn for_invite(identity: &Identity, node: Node, invite: &Invite) -> Self {
        let rendezvous = invite.relay.as_deref().and_then(RelayAddrs::parse);
        Self::start_guest(identity, node, invite, rendezvous).await
    }

    /// Starts a joiner's node for the call an invite names, with `rendezvous` as the
    /// rendezvous node. It registers in the host's room with the invite's lobby pass.
    pub async fn for_invite_via(
        identity: &Identity,
        node: Node,
        invite: &Invite,
        rendezvous: Option<NodeAddr>,
    ) -> Self {
        Self::start_guest(identity, node, invite, rendezvous.map(RelayAddrs::from)).await
    }

    async fn start_guest(
        identity: &Identity,
        node: Node,
        invite: &Invite,
        rendezvous: Option<RelayAddrs>,
    ) -> Self {
        let seat = Seat::Guest {
            host: invite.host,
            pass: lobby_pass(&invite.join_token),
        };
        Self::launch(identity, node, invite.room_id, rendezvous, None, seat).await
    }

    pub fn id(&self) -> PublicId {
        self.identity.public()
    }

    pub fn local_addr(&self) -> Result<SocketAddr, CallError> {
        Ok(self.node.local_addr()?)
    }

    pub fn room_id(&self) -> [u8; 16] {
        self.room_id
    }

    /// True while registered with a rendezvous node.
    pub fn has_rendezvous(&self) -> bool {
        self.presence.is_some()
    }

    /// When set, this node dials other participants only through the relay, never directly
    /// or by punching, and refuses their introductions, so they never learn its address.
    /// Costs relay bandwidth and latency. Dialling follows it in [`crate::join`] and in a
    /// media session made after the change; refusing introductions starts at once.
    pub fn set_relay_only(&mut self, relay_only: bool) {
        self.relay_only = relay_only;
        self.consent
            .relay_only
            .store(relay_only, std::sync::atomic::Ordering::Relaxed);
    }

    /// The media session for this call. There is one per node; a second call is refused.
    pub fn media(&mut self) -> Result<media::MediaSession, CallError> {
        let inbox = self.media.take().ok_or(CallError::Protocol("one media session per node"))?;
        Ok(media::MediaSession::new(
            self.node.clone(),
            self.identity.public(),
            self.room_id,
            self.presence.clone(),
            self.relay_only,
            inbox,
        ))
    }

    /// Asks `server` to dial this node's public IP at `port` from outside, over a link of its
    /// own (`veil_rendezvous::reach`). Owns what it needs, so it can run on a task of its own.
    pub fn check_reach(
        &self,
        server: NodeAddr,
        port: u16,
    ) -> impl Future<Output = Result<veil_rendezvous::Reach, CallError>> + Send + 'static {
        let node = self.node.clone();
        async move {
            veil_rendezvous::check_reach(&node, &server, port)
                .await
                .map_err(CallError::Rendezvous)
        }
    }

    /// Host only: leaves `full_link` sealed on the call's rendezvous node `relay`, and returns
    /// the short link that fetches it. Fails if the node cannot take it (too old, full, or not
    /// answering); the full link still works then.
    pub async fn shorten_invite(
        &self,
        relay: &RelayAddrs,
        full_link: &str,
    ) -> Result<String, CallError> {
        if !self.has_rendezvous() {
            return Err(CallError::Protocol("no rendezvous to hold the invite"));
        }
        let (short, sealed) = ShortInvite::create(full_link, &relay.addrs, &mut UnwrapErr(SysRng))
            .map_err(|_| CallError::Protocol("cannot seal the invite"))?;
        let tag = room_tag(&self.room_id);
        let store = store_invite(&self.node, &relay.addrs, relay.key, tag, sealed);
        tokio::time::timeout(SHORTEN_LIMIT, store)
            .await
            .map_err(|_| CallError::Timeout)?
            .map_err(CallError::Rendezvous)?;
        Ok(short.to_link())
    }

    /// This node's device key, which also signs its names and chat (`AppMessage::seal`).
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) async fn dial(
        &self,
        peer: PublicId,
        addrs: &[SocketAddr],
    ) -> Result<Dialled, CallError> {
        dial(&self.node, self.presence.as_ref(), peer, addrs, self.relay_only).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_tasks_stops_them() {
        let (tx, mut rx) = mpsc::channel::<()>(1);
        let mut tasks = Tasks::default();
        // Holds the sender until aborted, as the accept loop holds the node.
        tasks.0.push(tokio::spawn(async move {
            let _tx = tx;
            std::future::pending::<()>().await;
        }));
        drop(tasks);
        assert!(rx.recv().await.is_none(), "the task still holds its sender");
    }
}
