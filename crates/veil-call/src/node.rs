//! A participant's network presence for one call: its node, its router and its rendezvous.
//!
//! Purpose: every participant, host or joiner, starts a [`CallNode`]. It accepts links on the
//! node's socket and routes them (joiners to the host session, members to media), registers
//! with the call's rendezvous node if there is one, and dials peers along the best path that
//! works: direct, then a punched dial through an introduction, then a relay session.
//!
//! Invariants:
//! - Every dial names the key it expects, on every path. A relayed link is still the two
//!   participants' own pinned QUIC link.
//! - A rendezvous node that cannot be reached is not fatal: the call still works over direct
//!   addresses, and [`CallNode::has_rendezvous`] reports the loss.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use veil_net::{Link, Node, NodeAddr};
use veil_rendezvous::punch;
use veil_rendezvous::socket::RELAY_PEER;
use veil_security::invite::Invite;
use veil_security::{Identity, PublicId};

use crate::host::HostHandle;
use crate::inbound::{Inbound, Router};
use crate::presence::Presence;
use crate::{CallError, media};

/// Incoming media links waiting for the media session.
const MEDIA_INBOX: usize = 32;

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

/// A dialled link and what keeps it running.
pub(crate) struct Dialled {
    pub link: Link,
    /// The virtual node of a relayed link. Must live as long as the link.
    pub via: Option<Arc<Node>>,
    pub path: Path,
}

pub struct CallNode {
    node: Arc<Node>,
    identity: Identity,
    room_id: [u8; 16],
    presence: Option<Presence>,
    relay_only: bool,
    media: Option<mpsc::Receiver<Inbound>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for CallNode {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl CallNode {
    /// Starts routing links on `node` for the call `room_id`. A host passes its session so
    /// joiners reach it; `rendezvous` is the call's rendezvous and relay node, if any.
    pub async fn start(
        identity: &Identity,
        node: Node,
        room_id: [u8; 16],
        rendezvous: Option<NodeAddr>,
        host: Option<HostHandle>,
    ) -> Self {
        let node = Arc::new(node);
        let (media_tx, media_rx) = mpsc::channel(MEDIA_INBOX);
        let router = Router {
            room_id,
            host,
            media: media_tx,
        };
        let mut tasks = vec![tokio::spawn(router.clone().accept_loop(node.clone(), false))];
        let mut presence = None;
        if let Some(server) = rendezvous
            && let Ok((handle, task)) = Presence::start(identity, node.clone(), server, router).await
        {
            presence = Some(handle);
            tasks.push(task);
        }
        Self {
            node,
            identity: Identity::from_secret(&identity.secret_bytes()),
            room_id,
            presence,
            relay_only: false,
            media: Some(media_rx),
            tasks,
        }
    }

    /// Starts a joiner's node for the call an invite names, using the invite's relay field as
    /// the rendezvous node when it holds a `key@ip:port` address.
    pub async fn for_invite(identity: &Identity, node: Node, invite: &Invite) -> Self {
        let rendezvous = invite.relay.as_deref().and_then(|r| r.parse::<NodeAddr>().ok());
        Self::start(identity, node, invite.room_id, rendezvous, None).await
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
    /// or by punching, so they never learn its address. Costs relay bandwidth and latency.
    /// Applies to [`crate::join`] and to a media session made after the change.
    pub fn set_relay_only(&mut self, relay_only: bool) {
        self.relay_only = relay_only;
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

    pub(crate) fn identity(&self) -> &Identity {
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

/// Dials `peer` by the first path that works: direct, punched, relayed. `relay_only` skips the
/// first two, so the peer never learns this node's address.
pub(crate) async fn dial(
    node: &Node,
    presence: Option<&Presence>,
    peer: PublicId,
    addrs: &[SocketAddr],
    relay_only: bool,
) -> Result<Dialled, CallError> {
    let mut last = CallError::Net(veil_net::NetError::NoAddresses);
    if !relay_only {
        if !addrs.is_empty() {
            match node.connect(addrs, peer).await {
                Ok(link) => return Ok(Dialled { link, via: None, path: Path::Direct }),
                Err(e) => last = e.into(),
            }
        }
        if let Some(presence) = presence {
            let punched = match presence.introduce(peer).await {
                Ok(plan) => punch(node, &plan).await.map_err(CallError::from),
                Err(e) => Err(e),
            };
            match punched {
                Ok(link) => return Ok(Dialled { link, via: None, path: Path::Punched }),
                Err(e) => last = e,
            }
        }
    }
    let Some(presence) = presence else {
        return Err(last);
    };
    let inner = presence.relay(peer).await?;
    let link = inner.connect(&[RELAY_PEER], peer).await?;
    Ok(Dialled {
        link,
        via: Some(inner),
        path: Path::Relayed,
    })
}
