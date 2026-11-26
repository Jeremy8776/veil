//! The handle a host application holds to its session's actor.
//!
//! Each method queues one [`Command`] for the actor in `host.rs`, which owns all session state.
//! Commands that need an answer carry a oneshot reply.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use tokio::sync::{mpsc, oneshot, watch};
use veil_group::{Group, Lobby};
use veil_net::Link;
use veil_security::{Identity, PublicId};
use veil_topology::Plan;

use super::hints::checked_candidates;
use super::{Actor, Allowed, COMMANDS, Command, HostEvent, HostHandle};
use crate::events::EventSender;
use crate::inbound::Inbound;
use crate::routes::{Directory, Routes};
use crate::{CallError, EpochKeys};

impl HostHandle {
    /// Creates the call's group and starts the session. The first event is the epoch 0 keys.
    /// Must be called inside a Tokio runtime.
    /// Events are bounded by [`crate::EVENT_BACKLOG`]. A slow consumer ends the call with
    /// [`HostEvent::Overloaded`]; dropping the receiver also stops the actor.
    pub fn start(
        identity: &Identity,
        room_id: [u8; 16],
    ) -> Result<(Self, mpsc::Receiver<HostEvent>), CallError> {
        let group = Group::create(identity, room_id)?;
        let (events, events_rx) = EventSender::channel();
        events.emit(HostEvent::Keys(EpochKeys::from_group(&group)?));
        let (commands, commands_rx) = mpsc::channel(COMMANDS);
        let (internal, internal_rx) = mpsc::channel(COMMANDS);
        let (allowed_tx, allowed) = watch::channel(Allowed::default());
        let actor = Actor {
            identity: Identity::from_secret(&identity.secret_bytes()),
            group,
            lobby: Lobby::new(room_id),
            links: HashMap::new(),
            events,
            internal,
            next_link: 0,
            allowed: allowed_tx,
            recovering: HashMap::new(),
            history: VecDeque::new(),
            latest_plan: None,
            addresses: Vec::new(),
            hints: HashMap::new(),
            checking: None,
            cut_off: Vec::new(),
        };
        tokio::spawn(actor.run(commands_rx, internal_rx));
        Ok((Self { commands, allowed }, events_rx))
    }

    /// Who may be in the host's rendezvous room, updated after every change.
    pub(crate) fn allowed(&self) -> watch::Receiver<Allowed> {
        self.allowed.clone()
    }

    async fn send(&self, command: Command) -> Result<(), CallError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| CallError::Closed)
    }

    /// Makes a signed invite link with a fresh one-time token for this room.
    pub async fn invite(
        &self,
        addrs: Vec<String>,
        relay: Option<String>,
        expires_unix: u64,
    ) -> Result<String, CallError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Invite {
            addrs,
            relay,
            expires_unix,
            reply,
        })
        .await?;
        answer.await.map_err(|_| CallError::Closed)?
    }

    /// Hands the session a joiner's link. Its first control message must be `Hello`.
    pub async fn add_link(&self, link: Link) -> Result<(), CallError> {
        self.send(Command::AddLink(link)).await
    }

    /// Hands the session a joiner whose `Hello` a router already read.
    pub(crate) async fn add_inbound(&self, inbound: Inbound) -> Result<(), CallError> {
        self.send(Command::AddInbound(inbound)).await
    }

    /// Answers a [`HostEvent::Knock`].
    pub async fn decide(&self, device: PublicId, admit: bool) -> Result<(), CallError> {
        self.send(Command::Decide { device, admit }).await
    }

    /// Signs this epoch's layout and sends it to every member. `directory` must list exactly
    /// the current members; a plan made for an earlier epoch is refused. Returns the host's
    /// own routes. Members get theirs as [`crate::MemberEvent::Routes`].
    pub async fn publish_plan(
        &self,
        plan: Plan,
        directory: Directory,
    ) -> Result<Routes, CallError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Publish {
            plan,
            directory,
            reply,
        })
        .await?;
        answer.await.map_err(|_| CallError::Closed)?
    }

    /// Tests a proposed plan without replacing any member's working routes.
    pub async fn probe_plan(
        &self,
        plan: Plan,
        directory: Directory,
        request: u64,
    ) -> Result<Routes, CallError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Probe {
            plan,
            directory,
            request,
            reply,
        })
        .await?;
        answer.await.map_err(|_| CallError::Closed)?
    }

    /// Shares bounded connection hints over each authenticated host-control connection.
    /// New members and resumed members receive the most recent snapshot as well.
    pub async fn set_addresses(&self, addrs: Vec<SocketAddr>) -> Result<(), CallError> {
        let addrs =
            checked_candidates(addrs).ok_or(CallError::Protocol("invalid host addresses"))?;
        self.send(Command::Addresses(addrs)).await
    }

    /// Turns the host's gate on (`true`, the default) or off. With it off, joiners with a valid
    /// invite come straight in ([`HostEvent::Entered`]); joiners already waiting keep waiting.
    /// Tokens stay single use, the seat limit holds, and removal works as before.
    pub async fn set_gate(&self, on: bool) -> Result<(), CallError> {
        self.send(Command::Gate(on)).await
    }

    /// Removes a member. Its link closes after it receives the commit.
    pub async fn remove(&self, device: PublicId) -> Result<(), CallError> {
        self.send(Command::Remove(device)).await
    }
}
