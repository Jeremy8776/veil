//! A dual-stack rendezvous on localhost: reflection in both families, IPv6 candidates passed
//! on in introductions and preferred by the punched dial, and a reachability check over IPv6.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use veil_net::{Node, NodeAddr};
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::{Ask, RendezvousClient, RendezvousServer, check_reach, punch, room_tag};
use veil_security::Identity;

/// A dual-stack server, or `None` where this machine has no IPv6.
fn start_server() -> Option<(u16, veil_security::PublicId)> {
    let node = Node::bind_any(&Identity::generate(), 0).unwrap();
    if !node.ipv6() {
        eprintln!("no IPv6 on this machine; skipped");
        return None;
    }
    let server = RendezvousServer::new(node);
    let port = server.local_addr().unwrap().port();
    let key = server.id();
    tokio::spawn(server.run());
    Some((port, key))
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f).await.expect("timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ipv6_candidates_are_introduced_and_win_the_punch() {
    let Some((port, key)) = start_server() else {
        return;
    };
    let v4_server = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let room = room_tag(&[6; 16]);

    // Both members reach the server over IPv4 and say they also listen on IPv6 loopback.
    let host_id = Identity::generate();
    let host_key = host_id.public();
    let host_node = Node::bind_any(&host_id, 0).unwrap();
    let mut host = RendezvousClient::connect(&host_node, &[v4_server], key).await.unwrap();
    let host_v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, host_node.local_addr().unwrap().port()));
    let observed = host.register(room, host_key, None, vec![host_v6]).await.unwrap();
    assert!(observed.is_ipv4(), "an IPv4 client is reflected as plain IPv4: {observed}");

    let guest_id = Identity::generate();
    let guest_key = guest_id.public();
    host.allow(vec![guest_key], Vec::new()).await.unwrap();
    let guest_node = Node::bind_any(&guest_id, 0).unwrap();
    let mut guest = RendezvousClient::connect(&guest_node, &[v4_server], key).await.unwrap();
    let guest_v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, guest_node.local_addr().unwrap().port()));
    guest.register(room, host_key, None, vec![guest_v6]).await.unwrap();

    let (plan, ()) = tokio::join!(within(host.introduce(guest_key)), async {
        let event = within(guest.next_event()).await.unwrap();
        assert_eq!(event, ServerMsg::Request { from: host_key, kind: Ask::Introduce });
        guest.accept(host_key, Ask::Introduce).await.unwrap();
    });
    let plan = plan.unwrap();
    assert!(plan.addrs.contains(&guest_v6), "the IPv6 candidate is passed on: {:?}", plan.addrs);

    tokio::spawn(async move { while guest_node.accept().await.is_some() {} });
    let link = within(punch(&host_node, &plan)).await.unwrap();
    assert_eq!(link.peer(), guest_key);
    assert!(link.remote_addr().is_ipv6(), "IPv6 wins: {}", link.remote_addr());
}

#[tokio::test]
async fn reach_is_checked_over_ipv6() {
    let Some((port, key)) = start_server() else {
        return;
    };
    let server = NodeAddr { key, addr: SocketAddr::from((Ipv6Addr::LOCALHOST, port)) };
    let me = Node::bind_any(&Identity::generate(), 0).unwrap();
    let my_port = me.local_addr().unwrap().port();
    let reach = check_reach(&me, &server, my_port).await.unwrap();
    assert_eq!(reach.addr, SocketAddr::from((Ipv6Addr::LOCALHOST, my_port)));
    assert!(reach.open, "the node's own port answers over IPv6");
}
