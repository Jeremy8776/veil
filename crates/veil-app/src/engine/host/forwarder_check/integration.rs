//! Real control sessions and forwarding probes; no capture, playback or device access.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use tokio::sync::mpsc;
use veil_call::{EpochKeys, HostEvent, MemberEvent, MemberHandle, join};
use veil_forward::{ForwarderServer, Limits};
use veil_net::Node;
use veil_rendezvous::RendezvousServer;
use veil_security::{Identity, invite::Invite};

use super::*;
use crate::engine::{session::Pipes, view::Shared};

const ROOM: [u8; 16] = [91; 16];

async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("bounded control operation")
}

fn node(identity: &Identity) -> Node {
    Node::bind(identity, "127.0.0.1:0".parse().unwrap()).unwrap()
}

struct Call {
    node: CallNode,
    host: HostHandle,
    events: mpsc::Receiver<HostEvent>,
    session: Session,
    roster: Vec<(u32, PublicId)>,
    guests: Vec<(CallNode, MemberHandle)>,
    relay: NodeAddr,
    forwarder: NodeAddr,
    servers: Vec<tokio::task::JoinHandle<()>>,
    request: u64,
    forwarded: bool,
}

impl Drop for Call {
    fn drop(&mut self) {
        for task in &self.servers {
            task.abort();
        }
    }
}

impl Call {
    async fn start() -> Self {
        let identity = Identity::generate();
        let rendezvous = RendezvousServer::new(node(&Identity::generate()));
        let relay = NodeAddr {
            key: rendezvous.id(),
            addr: rendezvous.local_addr().unwrap(),
        };
        let forwarder = ForwarderServer::new(node(&Identity::generate()), Limits::default());
        let forwarder_addr = NodeAddr {
            key: forwarder.id(),
            addr: forwarder.local_addr().unwrap(),
        };
        let servers = vec![
            tokio::spawn(rendezvous.run()),
            tokio::spawn(forwarder.run()),
        ];
        let (host, events) = HostHandle::start(&identity, ROOM).unwrap();
        let mut call =
            CallNode::start(&identity, node(&identity), ROOM, Some(relay), host.clone()).await;
        let (_, encoded) = mpsc::channel(1);
        let (_, sound) = mpsc::channel(1);
        let (_, keyframes) = mpsc::unbounded_channel();
        let (video_in, _) = std::sync::mpsc::sync_channel(1);
        let (audio_in, _) = std::sync::mpsc::sync_channel(1);
        let session = Session::new(
            call.media().unwrap(),
            &identity,
            identity.public(),
            "Host",
            Arc::new(Shared::new(eframe::egui::Context::default())),
            Pipes {
                encoded,
                sound,
                keyframes,
                video_in,
                audio_in,
                control: Default::default(),
                screen_control: Default::default(),
                data: Default::default(),
            },
        );
        let mut result = Self {
            node: call,
            host,
            events,
            session,
            roster: Vec::new(),
            guests: Vec::new(),
            relay,
            forwarder: forwarder_addr,
            servers,
            request: 0,
            forwarded: false,
        };
        let keys = result.keys().await;
        result.accept_keys(keys);
        result
    }

    async fn keys(&mut self) -> EpochKeys {
        loop {
            if let HostEvent::Keys(keys) = within(self.events.recv()).await.unwrap() {
                return keys;
            }
        }
    }

    fn accept_keys(&mut self, keys: EpochKeys) {
        self.roster = keys.roster.clone();
        self.session.on_keys(keys);
    }

    async fn admit(&mut self, relay_only: bool) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let link = self
            .host
            .invite(
                vec![self.node.local_addr().unwrap().to_string()],
                Some(self.relay.to_string()),
                now + 60,
            )
            .await
            .unwrap();
        let invite = Invite::from_link(&link, now).unwrap();
        let joining = tokio::spawn(async move {
            let identity = Identity::generate();
            let mut call = CallNode::for_invite(&identity, node(&identity), &invite).await;
            call.set_relay_only(relay_only);
            let member = join(&call, &invite).await.unwrap();
            (call, member)
        });
        loop {
            if let HostEvent::Knock(device) = within(self.events.recv()).await.unwrap() {
                self.host.decide(device, true).await.unwrap();
                break;
            }
        }
        self.guests.push(within(joining).await.unwrap());
        let keys = self.keys().await;
        self.accept_keys(keys);
        for (_, guest) in &mut self.guests {
            // Synchronise every guest to the new epoch and discard the previous plan.
            loop {
                if matches!(
                    within(guest.events.recv()).await,
                    Some(MemberEvent::Keys(_))
                ) {
                    break;
                }
            }
        }
    }

    async fn propose(&mut self) -> Option<PendingPlan> {
        checked_plan(
            &self.host,
            &self.node,
            &mut self.session,
            &self.roster,
            self.node.id(),
            &[self.node.local_addr().unwrap()],
            &HashMap::new(),
            Some(self.forwarder),
            &mut self.request,
            &mut self.forwarded,
        )
        .await
    }

    async fn finish(&mut self, pending: &mut Option<PendingPlan>) {
        let (request, ok) = within(own_check(pending)).await;
        let candidate = pending.as_mut().unwrap();
        candidate.own.take();
        candidate.checks.report(self.node.id(), request, ok);
        while pending.is_some() {
            finish_checked_plan(pending, &self.host, &mut self.session, &mut self.forwarded).await;
            if pending.is_none() {
                break;
            }
            if let HostEvent::PlanChecked {
                device,
                request,
                ok,
            } = within(self.events.recv()).await.unwrap()
            {
                pending.as_mut().unwrap().checks.report(device, request, ok);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_only_newcomer_returns_a_forwarded_call_to_mesh() {
    let mut call = Call::start().await;
    for _ in 0..4 {
        call.admit(false).await;
    }
    let mut pending = call.propose().await;
    assert!(pending.is_some());
    call.finish(&mut pending).await;
    assert!(call.forwarded, "all five members checked the forwarder");
    for (_, guest) in &mut call.guests {
        assert!(matches!(
            within(guest.events.recv()).await,
            Some(MemberEvent::Routes(Routes::Mesh { .. }))
        ));
        assert!(matches!(
            within(guest.events.recv()).await,
            Some(MemberEvent::Routes(Routes::Forwarder { .. }))
        ));
    }
    call.admit(true).await;
    let mut pending = call.propose().await;
    assert!(
        !call.forwarded,
        "membership changes publish mesh before checking again"
    );
    assert!(
        pending.is_some(),
        "newcomers must be checked even if the call was forwarded"
    );
    call.finish(&mut pending).await;
    assert!(
        !call.forwarded,
        "one privacy refusal prevents the whole switch"
    );
    for (_, guest) in &mut call.guests {
        assert!(matches!(
            within(guest.events.recv()).await,
            Some(MemberEvent::Routes(Routes::Mesh { .. }))
        ));
        assert!(
            guest.events.try_recv().is_err(),
            "no forwarded plan followed the refusal"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removal_during_probes_refuses_activation_with_the_old_roster() {
    let mut call = Call::start().await;
    for _ in 0..4 {
        call.admit(false).await;
    }
    let mut pending = call.propose().await;
    let removed = call.guests[3].0.id();
    call.host.remove(removed).await.unwrap();
    let keys = call.keys().await;
    call.accept_keys(keys);
    // Even if every old response succeeds, publication checks the ACTUAL current roster.
    let candidate = pending.as_mut().unwrap();
    for device in candidate.checks.required.clone() {
        candidate
            .checks
            .report(device, candidate.checks.request, true);
    }
    finish_checked_plan(
        &mut pending,
        &call.host,
        &mut call.session,
        &mut call.forwarded,
    )
    .await;
    assert!(pending.is_none());
    assert!(!call.forwarded);
    assert!(
        call.propose().await.is_none(),
        "four current members remain in mesh"
    );
}
