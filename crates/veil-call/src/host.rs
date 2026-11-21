//! The host's session: lobby admission and epoch changes over real links.
//!
//! One actor task owns the [`Group`] and [`Lobby`]. Each joiner's link runs two tasks: a reader
//! that forwards control messages to the actor, and a writer that sends what the actor queues.
//! When the actor drops a link's queue (refusal, removal), the writer flushes, waits briefly for
//! the other side to hang up, and closes the link.
//!
//! Message flow per joiner: `Hello` -> [`HostEvent::Knock`] -> [`HostHandle::decide`] ->
//! `Admitted` or `Refused` -> `KeyPackage` -> `Welcome` to the joiner and `Commit` to every
//! existing member -> [`HostEvent::Joined`] and [`HostEvent::Keys`].
//!
//! Rendezvous admission: after every change the actor publishes [`Allowed`] (members, joiners
//! in the lobby, and the lobby passes of open invites). The host's presence task hands it to
//! the rendezvous node, which then drops anyone it no longer covers.
//!
//! After each [`HostEvent::Keys`] the app plans the epoch's layout and calls
//! [`HostHandle::publish_plan`]. The actor checks the plan lists exactly this epoch's members,
//! signs it, and queues it behind the epoch's commit, so every member reads the commit first.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use tokio::sync::{mpsc, oneshot, watch};
use veil_group::{Group, GroupError, Knock, Lobby};
use veil_net::control::{Control, RefuseReason};
use veil_net::Link;
use veil_rendezvous::protocol::MAX_PASSES;
use veil_security::invite::Invite;
use veil_security::{Identity, PublicId};
use veil_topology::Plan;

use crate::inbound::Inbound;
use crate::routes::{Directory, Routes};
use crate::{CallError, CallPlan, EpochKeys, now_unix};

mod hints;
mod link;
mod recovery;

pub(crate) use hints::checked_candidates;
pub use hints::MAX_HINTS;
use link::{Opened, run_link};
pub use recovery::CONTROL_RECOVERY;
use recovery::REPLAY_COMMITS;

/// Control messages queued for one link. A member that falls this far behind is dropped.
const OUTBOX: usize = 72;
const COMMANDS: usize = 64;
const LINGER: Duration = Duration::from_secs(2);

/// Who the host lets into its room on the rendezvous node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowed {
    /// Group members and joiners waiting in, or approved from, the lobby.
    pub members: Vec<PublicId>,
    /// Lobby passes of the invites still open (`veil_security::invite::lobby_pass`).
    pub passes: Vec<[u8; 32]>,
}

#[derive(Debug)]
pub enum HostEvent {
    /// A joiner with a valid token is waiting. Answer with [`HostHandle::decide`].
    Knock(PublicId),
    Joined {
        device: PublicId,
        leaf: u32,
    },
    /// A member left or was removed.
    Left(PublicId),
    /// A joiner was turned away before or while joining, for example with a used invite.
    Refused(PublicId, RefuseReason),
    /// A new epoch started. Replace the media pipeline's frame keys.
    Keys(EpochKeys),
    /// The group refused an operation. The session carries on in its previous epoch.
    Error(GroupError),
    Addresses {
        device: PublicId,
        addrs: Vec<std::net::SocketAddr>,
    },
    PlanChecked {
        device: PublicId,
        request: u64,
        ok: bool,
    },
}

enum Command {
    Invite {
        addrs: Vec<String>,
        relay: Option<String>,
        expires_unix: u64,
        reply: oneshot::Sender<Result<String, CallError>>,
    },
    AddLink(Link),
    AddInbound(Inbound),
    Decide {
        device: PublicId,
        admit: bool,
    },
    Remove(PublicId),
    Addresses(Vec<SocketAddr>),
    Publish {
        plan: Plan,
        directory: Directory,
        reply: oneshot::Sender<Result<Routes, CallError>>,
    },
    Probe {
        plan: Plan,
        directory: Directory,
        request: u64,
        reply: oneshot::Sender<Result<Routes, CallError>>,
    },
}

/// Events from one link task. `u64` is the link's id, so a stale or duplicate link of a
/// device can never act for the device's current link.
enum Internal {
    Opened(PublicId, u64, mpsc::Sender<Control>, Control),
    Message(PublicId, u64, Control),
    Closed(PublicId, u64, bool),
}

/// Controls a running host session. Cheap to clone.
#[derive(Clone)]
pub struct HostHandle {
    commands: mpsc::Sender<Command>,
    allowed: watch::Receiver<Allowed>,
}

impl HostHandle {
    /// Creates the call's group and starts the session. The first event is the epoch 0 keys.
    /// Must be called inside a Tokio runtime.
    // shortcut: events are unbounded so a busy app can never stall the actor while the actor
    // waits on the app. Bound it once the app has a dedicated event reader.
    pub fn start(
        identity: &Identity,
        room_id: [u8; 16],
    ) -> Result<(Self, mpsc::UnboundedReceiver<HostEvent>), CallError> {
        let group = Group::create(identity, room_id)?;
        let (events, events_rx) = mpsc::unbounded_channel();
        let _ = events.send(HostEvent::Keys(EpochKeys::from_group(&group)?));
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

    /// Removes a member. Its link closes after it receives the commit.
    pub async fn remove(&self, device: PublicId) -> Result<(), CallError> {
        self.send(Command::Remove(device)).await
    }
}

struct Actor {
    identity: Identity,
    group: Group,
    lobby: Lobby,
    links: HashMap<PublicId, (u64, mpsc::Sender<Control>)>,
    events: mpsc::UnboundedSender<HostEvent>,
    internal: mpsc::Sender<Internal>,
    next_link: u64,
    allowed: watch::Sender<Allowed>,
    recovering: HashMap<PublicId, (u64, Instant)>,
    history: VecDeque<(u64, Vec<u8>)>,
    latest_plan: Option<(u64, Vec<u8>)>,
    addresses: Vec<SocketAddr>,
    /// Per member: when its last address update went out, and the newest one waiting.
    hints: HashMap<PublicId, (Instant, Option<Vec<SocketAddr>>)>,
    /// The forwarder check in flight, and the members yet to answer it. One answer each.
    checking: Option<(u64, std::collections::HashSet<PublicId>)>,
    /// Devices cut off for falling behind. They are removed, not offered recovery.
    cut_off: Vec<PublicId>,
}

impl Actor {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut internal: mpsc::Receiver<Internal>,
    ) {
        let mut expiry = tokio::time::interval(Duration::from_secs(1));
        expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = expiry.tick() => {
                    self.expire_recovery();
                    self.flush_hints();
                }
                command = commands.recv() => match command {
                    Some(command) => self.command(command),
                    // Every handle dropped: the call is over.
                    None => break,
                },
                Some(event) = internal.recv() => self.internal(event),
            }
            self.remove_cut_off();
            self.publish_allowed();
        }
        // Reliable explicit goodbye is delivered before stream closure. A stream FIN alone
        // is indistinguishable from a transport interruption to the recovering member.
        for device in self.others() {
            self.send_to(&device, Control::Ended);
        }
    }

    /// Publishes who may be in the rendezvous room, if that changed.
    fn publish_allowed(&mut self) {
        let mut members: Vec<PublicId> = self.group.members().into_iter().map(|(_, d)| d).collect();
        members.extend(self.lobby.seated());
        members.sort();
        members.dedup();
        let next = Allowed {
            members,
            passes: self.lobby.passes(now_unix(), MAX_PASSES),
        };
        self.allowed.send_if_modified(|current| {
            let changed = *current != next;
            if changed {
                *current = next;
            }
            changed
        });
    }

    fn emit(&self, event: HostEvent) {
        let _ = self.events.send(event);
    }

    fn emit_keys(&self) {
        match EpochKeys::from_group(&self.group) {
            Ok(keys) => self.emit(HostEvent::Keys(keys)),
            Err(e) => self.emit(HostEvent::Error(e)),
        }
    }

    /// Queues a message. A member whose queue is full is cut off; its link closing then
    /// removes it, so it can never silently miss a commit and drift out of the group.
    fn send_to(&mut self, device: &PublicId, message: Control) {
        let full = self
            .links
            .get(device)
            .is_some_and(|(_, outbox)| outbox.try_send(message).is_err());
        if full {
            // Falling this far behind ends membership, as before recovery existed. Removal
            // runs after the current step (`remove_cut_off`), so loops over members stay valid.
            self.links.remove(device);
            if !self.cut_off.contains(device) {
                self.cut_off.push(*device);
            }
        }
    }

    /// Refuses a joiner and drops its link.
    fn refuse(&mut self, device: &PublicId, reason: RefuseReason) {
        self.emit(HostEvent::Refused(*device, reason));
        self.send_to(device, Control::Refused(reason));
        self.links.remove(device);
        self.lobby.forget(device);
    }

    fn broadcast_commit(&mut self, commit: &[u8], to: &[PublicId]) {
        self.history
            .push_back((self.group.epoch(), commit.to_vec()));
        while self.history.len() > REPLAY_COMMITS {
            self.history.pop_front();
        }
        for device in to {
            self.send_to(device, Control::Commit(commit.to_vec()));
        }
    }

    /// Members other than the host, as of now.
    fn others(&self) -> Vec<PublicId> {
        let me = self.identity.public();
        self.group
            .members()
            .into_iter()
            .map(|(_, d)| d)
            .filter(|d| *d != me)
            .collect()
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Invite {
                addrs,
                relay,
                expires_unix,
                reply,
            } => {
                let _ = reply.send(self.invite(addrs, relay, expires_unix));
            }
            Command::AddLink(link) => {
                self.next_link = self.next_link.wrapping_add(1);
                tokio::spawn(run_link(link, None, self.next_link, self.internal.clone()));
            }
            Command::AddInbound(inbound) => {
                self.next_link = self.next_link.wrapping_add(1);
                let Inbound {
                    link,
                    out,
                    input,
                    first,
                    via,
                } = inbound;
                let opened = Opened {
                    out,
                    input,
                    first,
                    _via: via,
                };
                tokio::spawn(run_link(
                    link,
                    Some(opened),
                    self.next_link,
                    self.internal.clone(),
                ));
            }
            Command::Decide { device, admit } => {
                let knock = Knock { device };
                if admit {
                    match self.lobby.approve(knock) {
                        Ok(reply) => self.send_to(&device, reply),
                        Err(e) => self.emit(HostEvent::Error(e)),
                    }
                } else {
                    let reply = self.lobby.decline(knock);
                    self.send_to(&device, reply);
                    self.links.remove(&device);
                }
            }
            Command::Remove(device) => self.remove(device, true),
            Command::Addresses(addrs) => {
                self.addresses = addrs;
                for device in self.others() {
                    self.send_to(&device, Control::Addresses(self.addresses.clone()));
                }
            }
            Command::Publish {
                plan,
                directory,
                reply,
            } => {
                let _ = reply.send(self.publish(&plan, &directory));
            }
            Command::Probe {
                plan,
                directory,
                request,
                reply,
            } => {
                let _ = reply.send(self.probe(&plan, &directory, request));
            }
        }
    }

    fn publish(&mut self, plan: &Plan, directory: &Directory) -> Result<Routes, CallError> {
        let room_id = self.lobby.room_id();
        let epoch = self.group.epoch();
        let plan = CallPlan::from_plan(plan, directory, room_id, epoch, &self.identity)?;
        let members: Vec<PublicId> = self.group.members().into_iter().map(|(_, d)| d).collect();
        if !plan.covers(&members) {
            return Err(CallError::Plan(
                "the directory does not list this epoch's members",
            ));
        }
        let sealed = plan.seal(&self.identity)?;
        self.latest_plan = Some((epoch, sealed.clone()));
        for device in self.others() {
            self.send_to(&device, Control::Plan(sealed.clone()));
        }
        plan.routes_for(&self.identity.public())
    }

    fn probe(
        &mut self,
        plan: &Plan,
        directory: &Directory,
        request: u64,
    ) -> Result<Routes, CallError> {
        let proposed = CallPlan::from_plan(
            plan,
            directory,
            self.lobby.room_id(),
            self.group.epoch(),
            &self.identity,
        )?;
        let members: Vec<PublicId> = self.group.members().into_iter().map(|(_, d)| d).collect();
        if !proposed.covers(&members) {
            return Err(CallError::Plan(
                "the directory does not list this epoch's members",
            ));
        }
        let sealed = proposed.seal(&self.identity)?;
        // Only answers to this request, one per member asked, reach the engine.
        self.checking = Some((request, self.others().into_iter().collect()));
        for device in self.others() {
            self.send_to(
                &device,
                Control::ProbePlan {
                    request,
                    sealed: sealed.clone(),
                },
            );
        }
        proposed.routes_for(&self.identity.public())
    }

    fn invite(
        &mut self,
        addrs: Vec<String>,
        relay: Option<String>,
        expires_unix: u64,
    ) -> Result<String, CallError> {
        let mut invite = Invite::new(
            &self.identity,
            addrs,
            relay,
            expires_unix,
            &mut UnwrapErr(SysRng),
        )
        .map_err(GroupError::Security)?;
        // The invite names this call's room, not the fresh id `Invite::new` drew.
        invite.room_id = self.lobby.room_id();
        self.lobby.add_invite(&invite)?;
        Ok(invite
            .to_link(&self.identity)
            .map_err(GroupError::Security)?)
    }

    /// Removes a member and starts a new epoch. `tell_them` sends the removed member the commit
    /// first, so it learns it was removed rather than seeing a dropped link.
    fn remove(&mut self, device: PublicId, tell_them: bool) {
        self.recovering.remove(&device);
        self.hints.remove(&device);
        if let Some((_, waiting)) = self.checking.as_mut() {
            waiting.remove(&device);
        }
        match self.group.remove(&device) {
            Ok(commit) => {
                let mut to = self.others();
                if tell_them {
                    to.push(device);
                }
                self.broadcast_commit(&commit, &to);
                self.links.remove(&device);
                self.emit(HostEvent::Left(device));
                self.emit_keys();
            }
            Err(GroupError::NotMember) => {
                self.links.remove(&device);
                self.lobby.forget(&device);
            }
            Err(e) => self.emit(HostEvent::Error(e)),
        }
    }

    fn internal(&mut self, event: Internal) {
        match event {
            Internal::Opened(device, link, outbox, Control::Resume { room_id, epoch }) => {
                self.resume(device, link, outbox, room_id, epoch);
            }
            Internal::Opened(device, link, outbox, first) => {
                if let std::collections::hash_map::Entry::Vacant(entry) = self.links.entry(device) {
                    entry.insert((link, outbox));
                    // This link replaces any outage; the expiry timer must not remove it later.
                    self.recovering.remove(&device);
                    self.message(device, first);
                }
            }
            Internal::Closed(device, link, voluntary) if !self.stale(&device, link) => {
                self.links.remove(&device);
                if !voluntary && self.group.members().iter().any(|(_, id)| *id == device) {
                    self.recovering
                        .entry(device)
                        .or_insert((link, Instant::now() + CONTROL_RECOVERY));
                } else {
                    self.remove(device, false);
                }
            }
            Internal::Message(device, link, message)
                if !self.stale(&device, link) && self.links.contains_key(&device) =>
            {
                self.message(device, message);
            }
            _ => {}
        }
    }

    fn message(&mut self, device: PublicId, message: Control) {
        match message {
            Control::Hello { .. } => {
                match self.lobby.knock(&message, device, &self.group, now_unix()) {
                    Ok(knock) => self.emit(HostEvent::Knock(knock.device)),
                    Err(GroupError::Refused(reason)) => self.refuse(&device, reason),
                    Err(_) => self.refuse(&device, RefuseReason::DeclinedByHost),
                }
            }
            Control::KeyPackage(_) => {
                let existing = self.others();
                match self.lobby.admit(&mut self.group, &device, &message) {
                    Ok(admission) => {
                        self.broadcast_commit(&admission.commit, &existing);
                        self.send_to(&device, Control::Welcome(admission.welcome));
                        if !self.addresses.is_empty() {
                            self.send_to(&device, Control::Addresses(self.addresses.clone()));
                        }
                        let leaf = self
                            .group
                            .members()
                            .into_iter()
                            .find_map(|(leaf, d)| (d == device).then_some(leaf));
                        if let Some(leaf) = leaf {
                            self.emit(HostEvent::Joined { device, leaf });
                        }
                        self.emit_keys();
                    }
                    Err(GroupError::Refused(reason)) => self.refuse(&device, reason),
                    Err(e) => {
                        self.emit(HostEvent::Error(e));
                        self.refuse(&device, RefuseReason::DeclinedByHost);
                    }
                }
            }
            Control::Ping(n) => self.send_to(&device, Control::Pong(n)),
            Control::Addresses(addrs)
                if self.group.members().iter().any(|(_, id)| *id == device) =>
            {
                // `run_link` already filtered these against the link's observed address.
                if addrs.len() <= MAX_HINTS {
                    self.hint(device, addrs);
                }
            }
            Control::PlanChecked { request, ok } => {
                let asked = self.checking.as_mut().is_some_and(|(current, waiting)| {
                    *current == request && waiting.remove(&device)
                });
                if asked {
                    self.emit(HostEvent::PlanChecked {
                        device,
                        request,
                        ok,
                    });
                }
            }
            // Nothing else is meaningful from a joiner or member.
            _ => {}
        }
    }
}

