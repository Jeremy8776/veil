//! The host's loop, shared by the person who started the call and a member who took over in
//! a persistent call (`successor.rs`). `host.rs` sets it up; this runs it until the call
//! ends for this device.
//!
//! Persistent calls (ADR 0010): Leave, quitting and closing the window count as Automatic.
//! The handover runs on a task of its own, so media and the lobby keep going while the
//! successor gets ready. If nobody can take over, Leave ends the call as before; a choice in
//! the leave dialog that fails leaves this device host and says so.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{Receiver, UnboundedReceiver};
use tokio::task::JoinHandle;
use veil_call::host::{Choice, HandoverFailure};
use veil_call::{CallNode, HostEvent, HostHandle, RelayAddrs};
use veil_net::NodeAddr;
use veil_security::PublicId;

use super::forwarder_check::{PendingPlan, checked_plan, finish_checked_plan, own_check};
use super::{
    check, close_router, hosted_addresses, moderated, new_invite, next_router, next_v6,
    publish_addresses, until,
};
use crate::engine::helper_evidence::HelperWatch;
use crate::engine::pinhole::V6Port;
use crate::engine::plan::{NetConfig, advertised, lan_ip};
use crate::engine::reach::Reachability;
use crate::engine::router::{Event, PortStatus, Router};
use crate::engine::session::Session;
use crate::engine::view::{Knocker, Shared};
use crate::engine::{Command, short};
use crate::diagnostics::{Problem, State, Step};
use crate::model::handover::{Bar, Stage, Successor};
use crate::model::Action;

/// Everything the host's loop needs. Built by `host::run`, or by `successor::take_over`.
pub(in crate::engine) struct Hosting {
    pub config: NetConfig,
    pub me: PublicId,
    pub shared: Arc<Shared>,
    pub host: HostHandle,
    pub events: Receiver<HostEvent>,
    pub call: CallNode,
    pub session: Session,
    pub rendezvous: NodeAddr,
    /// The public node from Settings, if any; `None` when this device runs its own.
    pub configured: Option<NodeAddr>,
    pub configured6: Option<NodeAddr>,
    pub forwarder: Option<NodeAddr>,
    pub helper_watch: HelperWatch,
    pub server_down: bool,
    pub port: u16,
    pub addrs: Vec<SocketAddr>,
    pub relay: RelayAddrs,
    pub until_end: bool,
    pub expires: u64,
    pub router: Option<Router>,
    pub relay_router: Option<Router>,
    pub ports: PortStatus,
    pub v6_port: Option<V6Port>,
    pub at6: Option<SocketAddr>,
    pub reach: Reachability,
    pub persistent: bool,
}

/// The handover in flight, and whether Leave asked for it (then a failure ends the call).
struct Handing {
    task: JoinHandle<Result<PublicId, HandoverFailure>>,
    leaving: bool,
}

/// The successor's answer, or never while no handover runs.
async fn handed(handing: &mut Option<Handing>) -> Result<PublicId, HandoverFailure> {
    match handing {
        Some(h) => (&mut h.task)
            .await
            .unwrap_or(Err(HandoverFailure::Closed)),
        None => std::future::pending().await,
    }
}

impl Hosting {
    fn start_handover(&mut self, handing: &mut Option<Handing>, pick: Option<u32>, leaving: bool) {
        if handing.is_some() {
            return;
        }
        let pick = pick.and_then(|id| self.session.ids.device(id));
        let host = self.host.clone();
        *handing = Some(Handing {
            task: tokio::spawn(async move { host.hand_over(pick).await }),
            leaving,
        });
        self.shared
            .update(|view| view.handover.stage = Stage::Handing(String::new()));
    }

    async fn show_choices(&mut self, choices: Vec<Choice>) {
        let shown: Vec<Successor> = choices
            .into_iter()
            .map(|c| Successor {
                id: self.session.ids.id(c.device),
                name: self.session.name_for(&c.device),
                bar: match c.bar {
                    veil_call::host::successor::Bar::Good => Bar::Good,
                    veil_call::host::successor::Bar::Ok => Bar::Ok,
                    veil_call::host::successor::Bar::Poor => Bar::Poor,
                },
            })
            .collect();
        self.shared.update(|view| view.handover.choices = shown);
    }

    /// Runs the call until this device leaves it, ends it or hands it over.
    pub async fn run(mut self, commands: &mut UnboundedReceiver<Command>) -> Result<(), String> {
        let me = self.me;
        let mut knocks: Vec<(u32, PublicId)> = Vec::new();
        let mut roster = Vec::new();
        let mut peer_addrs: HashMap<PublicId, Vec<SocketAddr>> = HashMap::new();
        let mut pending_plan: Option<PendingPlan> = None;
        let mut request = 0u64;
        // Whether members' media currently runs through the forwarder, and whether an address
        // change is waiting for the next tick. Address changes replan at most once a tick.
        let mut forwarded = false;
        let mut replan = false;
        let mut router_addr: Option<SocketAddr> = None;
        let mut handing: Option<Handing> = None;
        let mut refresh = tokio::time::interval(Duration::from_secs(2));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.shared.update(|view| {
            view.hosting = true;
            view.handover.persistent = self.persistent;
        });
        let local_only = self.config.local_only;
        // Once the interface lets go (quitting), only a handover is left to finish.
        let mut listening = true;
        let result = loop {
            tokio::select! {
                command = commands.recv(), if listening => match command {
                    // Leave, quit and closing the window: Automatic in a persistent call.
                    None | Some(Command::Leave) if self.persistent => {
                        listening = command.is_some();
                        match handing.as_mut() {
                            Some(h) => h.leaving = true,
                            None => self.start_handover(&mut handing, None, true),
                        }
                    }
                    None | Some(Command::Leave) | Some(Command::Act(Action::EndCall)) => break Ok(()),
                    Some(Command::Act(Action::HandOver(pick))) if self.persistent => {
                        self.start_handover(&mut handing, pick, false);
                    }
                    Some(Command::Act(said @ (Action::Chat(..) | Action::React(..)))) => {
                        self.session.say(&said).await;
                    }
                    Some(Command::Act(Action::Hand(raised))) => self.session.set_hand(raised).await,
                    // A fresh link with the chosen lifetime; one already sent keeps its own.
                    Some(Command::Act(Action::InviteUntilEnd(chosen))) => {
                        self.until_end = chosen;
                        self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, local_only, &self.shared).await;
                    }
                    Some(Command::Act(action)) => moderated::act(&self.host, &mut self.session, &mut knocks, action).await,
                },
                answer = handed(&mut handing) => {
                    let leaving = handing.take().is_some_and(|h| h.leaving);
                    match answer {
                        Ok(to) => {
                            let name = self.session.name_for(&to);
                            crate::note!("call: handing over to {}", short(&to));
                            self.shared.update(|view| view.handover.stage = Stage::Handing(name));
                        }
                        // Leave with nobody to take over ends the call, as without persistence.
                        Err(_) if leaving => break Ok(()),
                        Err(why) => {
                            crate::note!("call: could not hand over: {why:?}");
                            self.shared.update(|view| view.handover.stage = Stage::Failed);
                        }
                    }
                }
                event = self.events.recv() => match event {
                    None => break Err("The call stopped unexpectedly.".into()),
                    Some(HostEvent::Overloaded) => {
                        self.shared.diagnose(|d| d.set(Step::Membership, State::Failed(Problem::Overloaded)));
                        break Err("The call became too busy to keep up. Start a new call.".into());
                    },
                    Some(HostEvent::HandedOver { to }) => {
                        let name = self.session.name_for(&to);
                        self.shared.update(|view| view.handover.notice = Some(format!("{name} is now the host.")));
                        break Ok(());
                    }
                    Some(HostEvent::Knock(device)) => self.knock(&mut knocks, device, false).await,
                    Some(HostEvent::KnockRemovedEarlier(device)) => self.knock(&mut knocks, device, true).await,
                    // The lobby is off: the guest came straight in. Its token is burnt all the same.
                    Some(HostEvent::Entered(device)) => {
                        crate::note!("call: {} came in with the lobby off", short(&device));
                        self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, local_only, &self.shared).await;
                    }
                    // Joins and leaves go to the log, so a call across networks shows when the
                    // host heard of each.
                    Some(HostEvent::Joined { device, .. }) => {
                        crate::note!("call: {} joined", short(&device));
                        let id = self.session.ids.id(device);
                        knocks.retain(|(_, d)| *d != device);
                        self.shared.update(|view| view.lobby.retain(|k| k.id != id));
                    }
                    Some(HostEvent::Left(device)) => {
                        crate::note!("call: {} left", short(&device));
                        let id = self.session.ids.id(device);
                        knocks.retain(|(_, d)| *d != device);
                        self.shared.update(|view| view.lobby.retain(|k| k.id != id));
                        peer_addrs.remove(&device);
                    }
                    Some(HostEvent::Keys(keys)) => {
                        self.shared.diagnose(|d| d.set(Step::Membership, State::Passed));
                        roster = keys.roster.clone();
                        self.session.on_keys(keys);
                        peer_addrs.retain(|device, _| roster.iter().any(|(_, d)| d == device));
                        replan = false;
                        pending_plan = checked_plan(&self.host, &self.call, &mut self.session, &roster, me,
                            &self.addrs, &peer_addrs, self.forwarder, &mut request, &mut forwarded).await;
                    }
                    Some(HostEvent::Addresses { device, addrs: candidate }) => {
                        // Only current members' hints are kept, so this map is bounded by the
                        // roster. Guests send hints only in a Local only call. Outside one, a
                        // hint would spread a member's private addresses across the internet.
                        if local_only
                            && roster.iter().any(|(_, d)| *d == device)
                            && peer_addrs.get(&device) != Some(&candidate)
                        {
                            peer_addrs.insert(device, candidate);
                            replan = true;
                        }
                    }
                    Some(HostEvent::PlanChecked { device, request, ok }) => {
                        if let Some(pending) = pending_plan.as_mut() {
                            pending.checks.report(device, request, ok);
                        }
                        finish_checked_plan(&mut pending_plan, &self.host, &mut self.session, &mut forwarded).await;
                    }
                    Some(HostEvent::Refused(device, reason)) => {
                        crate::note!("call: turned {} away: {reason:?}", short(&device));
                    }
                    Some(HostEvent::Error(e)) => crate::note!("group error: {e}"),
                },
                (request, ok) = own_check(&mut pending_plan) => {
                    if let Some(pending) = pending_plan.as_mut() {
                        pending.own.take();
                        pending.checks.report(me, request, ok);
                    }
                    finish_checked_plan(&mut pending_plan, &self.host, &mut self.session, &mut forwarded).await;
                }
                _ = refresh.tick() => {
                    // Evidence follows actual transitions and keeps the latest typed failure kind.
                    if self.helper_watch.refresh(&self.shared, &self.call) == self.server_down {
                        self.server_down = !self.server_down;
                        let rendezvous = self.rendezvous;
                        if self.server_down {
                            crate::note!("rendezvous {rendezvous} stopped answering; still trying");
                        } else {
                            crate::note!("rendezvous {rendezvous} answering again");
                        }
                        let down = self.server_down;
                        self.shared.update(|view| view.server_down = down);
                    }
                    if self.persistent && handing.is_none()
                        && let Ok(choices) = self.host.choices().await
                    {
                        self.show_choices(choices).await;
                    }
                    finish_checked_plan(&mut pending_plan, &self.host, &mut self.session, &mut forwarded).await;
                    if replan {
                        replan = false;
                        pending_plan = checked_plan(&self.host, &self.call, &mut self.session, &roster, me,
                            &self.addrs, &peer_addrs, self.forwarder, &mut request, &mut forwarded).await;
                    }
                    let current_v6 = if local_only { None } else { veil_net::family::global_ipv6() };
                    let mut current = advertised(&self.config, current_v6, lan_ip(), self.port);
                    if let Some(mapped) = router_addr {
                        current.retain(|at| *at != mapped);
                        current.insert(0, mapped);
                        current.truncate(veil_security::invite::MAX_ADDRS);
                    }
                    if current != self.addrs {
                        self.addrs = current;
                        publish_addresses(&self.host, &self.addrs).await;
                        self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, local_only, &self.shared).await;
                        pending_plan = checked_plan(&self.host, &self.call, &mut self.session, &roster, me,
                            &self.addrs, &peer_addrs, self.forwarder, &mut request, &mut forwarded).await;
                    }
                }
                () = self.session.pump() => {}
                event = next_router(&mut self.router) => {
                    let open = match &event {
                        Event::Open(addr) => Some(*addr),
                        Event::Failed(_) => None,
                    };
                    self.ports.call_changed(event);
                    // The router's address goes first in every invite from now on.
                    let changed = open != router_addr;
                    if changed {
                        self.addrs.retain(|a| Some(*a) != router_addr);
                        if let Some(addr) = open {
                            self.addrs.retain(|a| *a != addr);
                            self.addrs.insert(0, addr);
                            self.addrs.truncate(veil_security::invite::MAX_ADDRS);
                        }
                        router_addr = open;
                        publish_addresses(&self.host, &self.addrs).await;
                        pending_plan = checked_plan(&self.host, &self.call, &mut self.session, &roster, me,
                            &self.addrs, &peer_addrs, self.forwarder, &mut request, &mut forwarded).await;
                    }
                    let checking = open.is_some_and(|at| check(&mut self.reach, &self.call, self.configured, at));
                    let (ports, reach) = (self.ports.view(), self.reach.shown());
                    self.shared.update(|view| {
                        view.router = ports;
                        if checking {
                            view.reach = reach;
                        }
                    });
                    if self.ports.settled() {
                        self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, local_only, &self.shared).await;
                    }
                }
                event = next_router(&mut self.relay_router) => {
                    // The public address first, for guests on the internet; the LAN address
                    // stays for guests on this network, since many routers do not loop back.
                    let public = match &event {
                        Event::Open(addr) => Some(*addr),
                        Event::Failed(_) => None,
                    };
                    self.ports.rendezvous_changed(event);
                    if let Some(addr) = public {
                        crate::note!("rendezvous: guests on the internet reach it at {addr}");
                    }
                    self.relay.addrs = hosted_addresses(self.rendezvous, public, local_only);
                    let ports = self.ports.view();
                    self.shared.update(|view| view.router = ports);
                    if self.ports.settled() {
                        self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, local_only, &self.shared).await;
                    }
                }
                // The link shown always works: one that ran out unused is replaced.
                () = until(self.expires) => {
                    self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, local_only, &self.shared).await;
                }
                // Whether or not the pinhole opened, the IPv6 address can be checked now.
                _ = next_v6(&mut self.v6_port) => {
                    if self.at6.is_some_and(|at| check(&mut self.reach, &self.call, self.configured6, at)) {
                        let reach = self.reach.shown();
                        self.shared.update(|view| view.reach = reach);
                    }
                }
                verdict = self.reach.next() => self.shared.update(|view| view.reach = verdict),
            }
            moderated::carry_out(&self.host, &mut self.session, &mut knocks).await;
        };
        let close_v6 = async {
            if let Some(v6_port) = self.v6_port {
                v6_port.close().await;
            }
        };
        // Together, because leaving waits about a second and each close may take 0.8 s of it.
        tokio::join!(close_router(self.router), close_router(self.relay_router), close_v6);
        result
    }

    async fn knock(&mut self, knocks: &mut Vec<(u32, PublicId)>, device: PublicId, removed_earlier: bool) {
        let id = self.session.ids.id(device);
        knocks.push((id, device));
        let knocker = Knocker {
            id,
            device,
            name: self.session.name_for(&device),
            safety: self.session.safety_with(&device),
            since: Instant::now(),
        };
        self.shared.update(|view| {
            view.lobby.push(knocker);
            if removed_earlier && !view.handover.removed_earlier.contains(&id) {
                view.handover.removed_earlier.push(id);
            }
        });
        // The knock used the invite's token: give out a fresh one.
        self.expires = new_invite(&self.host, &self.call, &self.addrs, &self.relay, self.until_end, self.config.local_only, &self.shared).await;
    }
}
