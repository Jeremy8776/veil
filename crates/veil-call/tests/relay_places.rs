//! Relay sessions nobody uses must not lock relayed guests out of a call. Real rendezvous,
//! host and guests on localhost; the invite holder speaks to the rendezvous directly, and
//! never knocks or dials.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use veil_call::{CallError, CallNode, HostEvent, HostHandle, MemberHandle, Path, join};
use veil_net::{Node, NodeAddr};
use veil_rendezvous::{RendezvousClient, RendezvousError, RendezvousServer, room_tag};
use veil_security::invite::{Invite, lobby_pass};
use veil_security::{Identity, PublicId};

const ROOM: [u8; 16] = [0x52; 16];
/// The most relay sessions the server lets one member hold.
const FULL_LINK: usize = 16;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Host {
    _node: CallNode,
    handle: HostHandle,
    events: Receiver<HostEvent>,
    key: PublicId,
    addr: SocketAddr,
    rendezvous: NodeAddr,
}

async fn start_host() -> Host {
    let server = RendezvousServer::new(Node::bind(&Identity::generate(), localhost()).unwrap());
    let rendezvous = NodeAddr { key: server.id(), addr: server.local_addr().unwrap() };
    tokio::spawn(server.run());

    let identity = Identity::generate();
    let node = Node::bind(&identity, localhost()).unwrap();
    let addr = node.local_addr().unwrap();
    let (handle, events) = HostHandle::start(&identity, ROOM).unwrap();
    let node = CallNode::start(&identity, node, ROOM, Some(rendezvous), handle.clone()).await;
    assert!(node.has_rendezvous());
    Host { _node: node, handle, events, key: identity.public(), addr, rendezvous }
}

impl Host {
    async fn invite(&self) -> Invite {
        let relay = Some(self.rendezvous.to_string());
        let link = self.handle.invite(vec![self.addr.to_string()], relay, now() + 600).await.unwrap();
        Invite::from_link(&link, now()).unwrap()
    }

    /// A guest that can reach the host only through the relay. The host admits it.
    async fn relay_only_guest(&mut self) -> (CallNode, Result<MemberHandle, CallError>) {
        let invite = self.invite().await;
        let mut joining = tokio::spawn(async move {
            let identity = Identity::generate();
            let node = Node::bind(&identity, localhost()).unwrap();
            let mut node = CallNode::for_invite(&identity, node, &invite).await;
            assert!(node.has_rendezvous());
            node.set_relay_only(true);
            let result = join(&node, &invite).await;
            (node, result)
        });
        let run = async {
            loop {
                tokio::select! {
                    done = &mut joining => return done.unwrap(),
                    event = self.events.recv() => match event.unwrap() {
                        HostEvent::Knock(device) => self.handle.decide(device, true).await.unwrap(),
                        HostEvent::Error(e) => panic!("host error: {e}"),
                        _ => {}
                    },
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(30), run).await.expect("timed out")
    }
}

/// Registers `identity` with the invite's lobby pass. Retries while the host's list is on its
/// way, or the pass is still held by this identity's previous link.
async fn register(host: &Host, identity: &Identity, invite: &Invite) -> (Node, RendezvousClient) {
    for _ in 0..100 {
        let node = Node::bind(identity, localhost()).unwrap();
        let mut client = RendezvousClient::connect(&node, &[host.rendezvous.addr], host.rendezvous.key)
            .await
            .unwrap();
        let pass = Some(lobby_pass(&invite.join_token));
        match client.register(room_tag(&invite.room_id), invite.host, pass, Vec::new()).await {
            Ok(_) => return (node, client),
            Err(RendezvousError::Refused(_)) => {
                drop(client);
                node.close();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("register: {e:?}"),
        }
    }
    panic!("the rendezvous never let the invite holder in");
}

/// Opens as many sessions with the host as one rendezvous link may hold, and uses none.
async fn fill_link(host: &Host, client: &mut RendezvousClient) {
    for i in 0..FULL_LINK {
        client.open_relay(host.key).await.unwrap_or_else(|e| panic!("request {i}: {e:?}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_left_unused_do_not_lock_out_a_relayed_guest() {
    let mut host = start_host().await;
    let invite = host.invite().await;
    let holder = Identity::generate();
    for _ in 0..2 {
        let (node, mut client) = register(&host, &holder, &invite).await;
        fill_link(&host, &mut client).await;
        drop(client);
        node.close();
    }

    for _ in 0..2 {
        let (_node, joined) = host.relay_only_guest().await;
        assert_eq!(joined.expect("the guest joins").path(), Path::Relayed);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_holder_that_stays_connected_cannot_fill_the_hosts_sessions() {
    let mut host = start_host().await;
    let invite = host.invite().await;
    let (_held_node, mut held) = register(&host, &Identity::generate(), &invite).await;
    fill_link(&host, &mut held).await;

    let (_node, joined) = host.relay_only_guest().await;
    assert_eq!(joined.expect("the guest joins").path(), Path::Relayed);
}
