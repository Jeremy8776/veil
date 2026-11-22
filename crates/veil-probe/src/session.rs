//! One participant's probe: applies keys and routes as the call sends them, pings every other
//! member once a second on both channels, answers their pings and reports what came back.
//!
//! Invariants:
//! - Every probe is sealed with the current epoch's sender and opened through a [`KeyRing`],
//!   so a reply proves the end-to-end keys work, not only the link.
//! - The true sender is the frame's authenticated sender index, never the link it came on.

use std::time::{Duration, Instant};

use veil_call::{EpochKeys, Incoming, KeyRing, Media, MediaSession, Routes};
use veil_security::PublicId;
use veil_security::frame::FrameSender;

use crate::wire::{Channel, Kind, Probe, Stats};

pub struct Session {
    pub media: MediaSession,
    me: PublicId,
    leaf: Option<u32>,
    sender: Option<FrameSender>,
    ring: Option<KeyRing>,
    seq: u32,
    clock: Instant,
    stats: Stats,
}

impl Session {
    pub fn new(media: MediaSession, me: PublicId) -> Self {
        Self {
            media,
            me,
            leaf: None,
            sender: None,
            ring: None,
            seq: 0,
            clock: Instant::now(),
            stats: Stats::default(),
        }
    }

    pub fn on_keys(&mut self, keys: EpochKeys) {
        self.media.members(keys.roster.iter().map(|(_, d)| *d));
        self.leaf = keys
            .roster
            .iter()
            .find(|(_, d)| *d == self.me)
            .map(|(leaf, _)| *leaf);
        println!(
            "epoch {}: {} member(s), you are member {}",
            keys.epoch,
            keys.roster.len(),
            self.leaf.map_or_else(|| "?".into(), |l| l.to_string()),
        );
        let roster = keys.roster.iter().copied();
        match &mut self.ring {
            Some(ring) => ring.rotate(keys.epoch, keys.receiver, roster, Instant::now()),
            None => self.ring = Some(KeyRing::new(keys.epoch, keys.receiver, roster)),
        }
        self.sender = Some(keys.sender);
    }

    pub async fn on_routes(&mut self, routes: Routes) {
        for (peer, e) in self.media.apply(routes).await {
            println!("  could not reach {}: {e}", short(&peer));
        }
        // Inbound links from members that dial us arrive shortly after; report once they have.
        if self.media.ready(Duration::from_secs(10)).await.is_err() {
            println!("  not every expected link arrived within 10 s");
        }
        for (peer, link) in self.media.link_stats() {
            let family = if link.ipv6 { " over IPv6" } else { "" };
            println!("  link to {}: {:?}{family}", short(&peer), link.path);
        }
    }

    /// Sends one ping as a video frame and one as an audio datagram.
    pub async fn ping(&mut self) {
        let Some(leaf) = self.leaf else { return };
        self.seq = self.seq.wrapping_add(1);
        let ping = Probe {
            kind: Kind::Ping,
            seq: self.seq,
            pinger: leaf,
            stamp_us: self.now_us(),
        };
        self.send(ping, Channel::Video).await;
        self.send(ping, Channel::Audio).await;
        self.stats.sent();
    }

    pub async fn on_incoming(&mut self, incoming: Incoming) {
        let (channel, packet) = match &incoming.media {
            Media::Video { sealed, .. } => (Channel::Video, sealed.as_slice()),
            Media::Audio(bytes) => (Channel::Audio, bytes.as_ref()),
            Media::KeyframeRequest | Media::App(_) => return,
        };
        let Some(ring) = &mut self.ring else { return };
        let (from, plain) = match ring.open_from(packet, &incoming.via, Instant::now()) {
            Ok(opened) => opened,
            Err(e) => {
                println!(
                    "  a {channel:?} packet via {} did not open: {e}",
                    short(&incoming.via)
                );
                return;
            }
        };
        let Some(probe) = Probe::decode(&plain) else {
            return;
        };
        match probe.kind {
            Kind::Ping => self.send(probe.pong(), channel).await,
            Kind::Pong if Some(probe.pinger) == self.leaf => {
                let rtt = self.now_us().saturating_sub(probe.stamp_us);
                self.stats.reply(from, channel, Duration::from_micros(rtt));
            }
            Kind::Pong => {}
        }
    }

    /// The reply lines since the last report, then a new window.
    pub fn report(&mut self) -> Vec<String> {
        self.stats.report()
    }

    async fn send(&mut self, probe: Probe, channel: Channel) {
        let Some(sender) = &mut self.sender else {
            return;
        };
        let sealed = match sender.encrypt(&probe.encode()) {
            Ok(sealed) => sealed,
            Err(e) => {
                println!("  cannot seal a probe: {e}");
                return;
            }
        };
        match channel {
            Channel::Video => {
                self.media.send_frame(probe.seq, &sealed).await;
            }
            Channel::Audio => {
                self.media.send_audio(&sealed);
            }
        }
    }

    fn now_us(&self) -> u64 {
        u64::try_from(self.clock.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// The first 8 hex digits of a key, enough to tell members apart in a log.
pub fn short(key: &PublicId) -> String {
    key.0.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tokio::sync::mpsc::UnboundedReceiver;
    use veil_call::{CallNode, HostEvent, HostHandle, MemberEvent, join};
    use veil_net::{Node, NodeAddr};
    use veil_rendezvous::RendezvousServer;
    use veil_security::Identity;
    use veil_security::invite::Invite;

    use super::*;
    use crate::args::HostArgs;
    use crate::host::plan_for;

    const ROOM: [u8; 16] = [9; 16];

    async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(20), f)
            .await
            .expect("timed out")
    }

    fn localhost() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    async fn host_keys(events: &mut UnboundedReceiver<HostEvent>) -> EpochKeys {
        loop {
            match within(events.recv()).await.unwrap() {
                HostEvent::Keys(keys) => return keys,
                HostEvent::Knock(_)
                | HostEvent::Joined { .. }
                | HostEvent::Left(_)
                | HostEvent::Addresses { .. }
                | HostEvent::PlanChecked { .. }
                | HostEvent::Refused(..) => {}
                HostEvent::Error(e) => panic!("host error: {e}"),
            }
        }
    }

    /// Handles incoming probes on both sides until neither has heard anything for a second.
    async fn exchange(a: &mut Session, b: &mut Session) {
        within(async {
            loop {
                tokio::select! {
                    Some(i) = a.media.recv() => a.on_incoming(i).await,
                    Some(i) = b.media.recv() => b.on_incoming(i).await,
                    () = tokio::time::sleep(Duration::from_secs(1)) => return,
                }
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_members_answer_each_others_probes_on_both_channels() {
        let server = RendezvousServer::new(Node::bind(&Identity::generate(), localhost()).unwrap());
        let rendezvous = NodeAddr {
            key: server.id(),
            addr: server.local_addr().unwrap(),
        };
        tokio::spawn(server.run());

        let me = Identity::generate();
        let node = Node::bind(&me, localhost()).unwrap();
        let addr = node.local_addr().unwrap();
        let (host, mut events) = HostHandle::start(&me, ROOM).unwrap();
        let mut call = CallNode::start(&me, node, ROOM, Some(rendezvous), host.clone()).await;
        let mut hosting = Session::new(call.media().unwrap(), me.public());
        hosting.on_keys(host_keys(&mut events).await);

        let link = host.invite(
            vec![addr.to_string()],
            Some(rendezvous.to_string()),
            now() + 600,
        );
        let invite = Invite::from_link(&within(link).await.unwrap(), now()).unwrap();
        let joining = tokio::spawn(async move {
            let id = Identity::generate();
            let node =
                CallNode::for_invite(&id, Node::bind(&id, localhost()).unwrap(), &invite).await;
            let member = join(&node, &invite).await.unwrap();
            (id, node, member)
        });
        loop {
            if let HostEvent::Knock(device) = within(events.recv()).await.unwrap() {
                host.decide(device, true).await.unwrap();
                break;
            }
        }
        let (id, mut node, mut member) = within(joining).await.unwrap();
        let mut joined = Session::new(node.media().unwrap(), id.public());
        let MemberEvent::Keys(keys) = within(member.events.recv()).await.unwrap() else {
            panic!("expected keys")
        };
        joined.on_keys(keys);
        let keys = host_keys(&mut events).await;
        let args = HostArgs {
            bind: localhost(),
            advertise: vec![addr],
            rendezvous: Some(rendezvous),
            forwarder: None,
        };
        let (plan, directory) = plan_for(&keys.roster, me.public(), &args).unwrap();
        hosting.on_keys(keys);

        let mine = host.publish_plan(plan, directory).await.unwrap();
        let MemberEvent::Routes(theirs) = within(member.events.recv()).await.unwrap() else {
            panic!("expected routes")
        };
        within(async { tokio::join!(hosting.on_routes(mine), joined.on_routes(theirs)) }).await;

        hosting.ping().await;
        joined.ping().await;
        exchange(&mut hosting, &mut joined).await;
        let host_heard = hosting.report();
        let member_heard = joined.report();
        assert_eq!(
            host_heard.len(),
            2,
            "video and audio replies: {host_heard:?}"
        );
        assert!(
            host_heard[0].starts_with("member 1 Video: 1/1"),
            "{host_heard:?}"
        );
        assert!(
            host_heard[1].starts_with("member 1 Audio: 1/1"),
            "{host_heard:?}"
        );
        assert!(
            member_heard[0].starts_with("member 0 Video: 1/1"),
            "{member_heard:?}"
        );
        drop((call, node));
    }
}
