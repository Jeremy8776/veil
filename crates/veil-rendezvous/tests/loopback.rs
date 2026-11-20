//! Rendezvous, introduction and relay between nodes on localhost.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::Node;
use veil_net::control::Control;
use veil_rendezvous::protocol::ServerMsg;
use veil_rendezvous::socket::RELAY_PEER;
use veil_rendezvous::{
    Ask, Punch, Refusal, RendezvousClient, RendezvousError, RendezvousServer, punch, room_tag,
};
use veil_security::frame::{FrameReceiver, FrameSender};
use veil_security::{Identity, PublicId};

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Starts a rendezvous server and returns its address and key.
fn start_server() -> (SocketAddr, PublicId) {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node);
    let addr = server.local_addr().unwrap();
    let key = server.id();
    tokio::spawn(server.run());
    (addr, key)
}

struct Member {
    identity: Identity,
    node: Node,
    client: RendezvousClient,
    /// For a host: the keys it has allowed into its room so far.
    allowed: Vec<PublicId>,
}

async fn register(
    server: (SocketAddr, PublicId),
    identity: Identity,
    room: [u8; 32],
    host: PublicId,
    pass: Option<[u8; 32]>,
) -> Result<Member, RendezvousError> {
    let node = Node::bind(&identity, localhost()).unwrap();
    let mut client = RendezvousClient::connect(&node, &[server.0], server.1).await.unwrap();
    let local = node.local_addr().unwrap();
    let observed = client.register(room, host, pass, vec![local]).await?;
    assert_eq!(observed, local, "server reflects the address it sees");
    Ok(Member {
        identity,
        node,
        client,
        allowed: Vec::new(),
    })
}

/// The room's host: it opens the room.
async fn host(server: (SocketAddr, PublicId), room: [u8; 32]) -> Member {
    let identity = Identity::generate();
    let key = identity.public();
    register(server, identity, room, key, None).await.unwrap()
}

/// Another member: the host allows its key first.
async fn guest(server: (SocketAddr, PublicId), room: [u8; 32], host: &mut Member) -> Member {
    let identity = Identity::generate();
    host.allowed.push(identity.public());
    host.client.allow(host.allowed.clone(), Vec::new()).await.unwrap();
    register(server, identity, room, host.identity.public(), None).await.unwrap()
}

/// `m` waits for `from`'s request of `kind` and accepts it.
async fn consent(m: &mut Member, from: PublicId, kind: Ask) {
    let event = with_timeout(m.client.next_event()).await.unwrap();
    assert_eq!(event, ServerMsg::Request { from, kind });
    m.client.accept(from, kind).await.unwrap();
}

async fn with_timeout<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn introduction_lets_two_members_connect_directly() {
    let server = start_server();
    let room = room_tag(&[1; 16]);
    let mut a = host(server, room).await;
    let mut b = guest(server, room, &mut a).await;
    let b_key = b.identity.public();
    let a_key = a.identity.public();

    let (plan_a, ()) = tokio::join!(
        with_timeout(a.client.introduce(b_key)),
        consent(&mut b, a_key, Ask::Introduce)
    );
    let plan_a = plan_a.unwrap();
    assert_eq!(plan_a.peer, b_key);
    let ServerMsg::Punch { peer, .. } = with_timeout(b.client.next_event()).await.unwrap() else {
        panic!("b should be told to punch");
    };
    assert_eq!(peer, a_key);

    // b would dial too; on localhost a's dial alone is enough. b's node accepts it.
    let accept = tokio::spawn(async move {
        loop {
            let link = b.node.accept().await.unwrap().unwrap();
            if link.peer() == a_key {
                return link.peer();
            }
        }
    });
    let link = with_timeout(punch(&a.node, &plan_a)).await.unwrap();
    assert_eq!(link.peer(), b_key);
    assert_eq!(with_timeout(accept).await.unwrap(), a_key);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_punch_does_not_wait_as_long_as_a_notice_says() {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let plan = Punch {
        peer: PublicId([1; 32]),
        addrs: Vec::new(),
        delay: Duration::from_secs(3_600),
    };
    // With no addresses the dial fails as soon as the wait is over.
    assert!(with_timeout(punch(&node, &plan)).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_of_another_room_are_not_introduced() {
    let server = start_server();
    let mut a = host(server, room_tag(&[1; 16])).await;
    let stranger = host(server, room_tag(&[2; 16])).await;
    // No answer at all, the same as a member that declines: the server does not say
    // whether the key is online.
    let result = with_timeout(a.client.introduce(stranger.identity.public())).await;
    assert!(matches!(result, Err(RendezvousError::Timeout)), "{result:?}");
    let relay = with_timeout(a.client.open_relay(stranger.identity.public())).await;
    assert!(matches!(relay, Err(RendezvousError::Timeout)), "{relay:?}");
    let me = with_timeout(a.client.introduce(a.identity.public())).await;
    assert!(matches!(me, Err(RendezvousError::Refused(Refusal::PeerNotInRoom))), "{me:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_carries_a_pinned_end_to_end_link() {
    let server = start_server();
    let room = room_tag(&[7; 16]);
    let mut a = host(server, room).await;
    let mut b = guest(server, room, &mut a).await;
    let a_key = a.identity.public();
    let b_key = b.identity.public();

    let (session, ()) = tokio::join!(
        with_timeout(a.client.open_relay(b_key)),
        consent(&mut b, a_key, Ask::Relay)
    );
    let session = session.unwrap();
    let ServerMsg::RelayReady { session: b_session, peer } =
        with_timeout(b.client.next_event()).await.unwrap()
    else {
        panic!("b should be told about the relay");
    };
    assert_eq!((b_session, peer), (session, a_key));

    // Each side binds a second node to its relay socket. Same identities, same pinning.
    let a_inner = Node::bind_virtual(&a.identity, a.client.relay_socket(session).unwrap()).unwrap();
    let b_inner = Node::bind_virtual(&b.identity, b.client.relay_socket(session).unwrap()).unwrap();

    let epoch_secret = [9u8; 32];
    let frame: Vec<u8> = (0..300_000u32).map(|i| (i % 249) as u8).collect();
    let expected = frame.clone();

    let callee = tokio::spawn(async move {
        let link = b_inner.accept().await.unwrap().unwrap();
        assert_eq!(link.peer(), a_key);
        let mut ctl = link.accept_control().await.unwrap();
        assert_eq!(
            ctl.recv().await.unwrap(),
            Control::Hello { room_id: [7; 16], join_token: [8; 16] }
        );
        ctl.send(&Control::Admitted).await.unwrap();
        let (_, sealed) = link.recv_frame().await.unwrap();
        let mut keys = FrameReceiver::new(&epoch_secret, 0).unwrap();
        keys.decrypt(&sealed).unwrap().1
    });

    let link = with_timeout(a_inner.connect(&[RELAY_PEER], b_key)).await.unwrap();
    assert_eq!(link.peer(), b_key);
    let mut ctl = link.open_control().await.unwrap();
    ctl.send(&Control::Hello { room_id: [7; 16], join_token: [8; 16] })
        .await
        .unwrap();
    assert_eq!(with_timeout(ctl.recv()).await.unwrap(), Control::Admitted);
    let sealed = FrameSender::new(&epoch_secret, 0, 1).unwrap().encrypt(&frame).unwrap();
    with_timeout(link.send_frame(0, &sealed)).await.unwrap();

    let got = with_timeout(callee).await.unwrap();
    assert_eq!(got, expected);
    link.close("done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relayed_dial_to_the_wrong_key_fails() {
    let server = start_server();
    let room = room_tag(&[5; 16]);
    let mut a = host(server, room).await;
    let mut b = guest(server, room, &mut a).await;
    let a_key = a.identity.public();
    let (session, ()) = tokio::join!(
        with_timeout(a.client.open_relay(b.identity.public())),
        consent(&mut b, a_key, Ask::Relay)
    );
    let session = session.unwrap();
    let _ = with_timeout(b.client.next_event()).await;

    let a_inner = Node::bind_virtual(&a.identity, a.client.relay_socket(session).unwrap()).unwrap();
    let b_inner = Node::bind_virtual(&b.identity, b.client.relay_socket(session).unwrap()).unwrap();
    tokio::spawn(async move { while b_inner.accept().await.is_some() {} });

    // The relay delivers to b, but a expected someone else: the handshake must fail.
    let someone_else = Identity::generate().public();
    let result = with_timeout(a_inner.connect(&[RELAY_PEER], someone_else)).await;
    assert!(result.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_can_be_turned_off() {
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node).without_relay();
    let addr = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());
    let room = room_tag(&[6; 16]);
    let mut a = host(addr, room).await;
    let mut b = guest(addr, room, &mut a).await;
    assert!(matches!(
        with_timeout(a.client.open_relay(b.identity.public())).await,
        Err(RendezvousError::Refused(Refusal::RelayDisabled))
    ));
    // Introductions still work.
    let a_key = a.identity.public();
    let (plan, ()) = tokio::join!(
        with_timeout(a.client.introduce(b.identity.public())),
        consent(&mut b, a_key, Ask::Introduce)
    );
    assert!(plan.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unanswered_request_reveals_nothing() {
    let server = start_server();
    let room = room_tag(&[8; 16]);
    let mut asker = host(server, room).await;
    let mut target = guest(server, room, &mut asker).await;
    let (asker_key, target_key) = (asker.identity.public(), target.identity.public());

    // The target is asked but does not accept: the asker gets no address, only a timeout.
    let result = with_timeout(asker.client.introduce(target_key)).await;
    assert!(matches!(result, Err(RendezvousError::Timeout)), "{result:?}");
    let event = with_timeout(target.client.next_event()).await.unwrap();
    assert_eq!(event, ServerMsg::Request { from: asker_key, kind: Ask::Introduce });

    // An accept for a request that was never made does nothing for the "asker".
    target.client.accept(asker_key, Ask::Relay).await.unwrap();
    let quiet = tokio::time::timeout(Duration::from_millis(300), asker.client.next_event()).await;
    assert!(quiet.is_err(), "nothing reaches the asker: {quiet:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_keys_the_host_allows_may_register() {
    let server = start_server();
    let room = room_tag(&[9; 16]);
    let mut owner = host(server, room).await;
    let owner_key = owner.identity.public();

    let stranger = register(server, Identity::generate(), room, owner_key, None).await;
    assert!(matches!(stranger, Err(RendezvousError::Refused(Refusal::NotAllowed))));
    let unhosted = register(server, Identity::generate(), room, PublicId([7; 32]), None).await;
    assert!(matches!(unhosted, Err(RendezvousError::Refused(Refusal::UnknownRoom))));

    // A lobby pass lets one key in at a time.
    let pass = [4; 32];
    owner.client.allow(Vec::new(), vec![pass]).await.unwrap();
    let joiner = register(server, Identity::generate(), room, owner_key, Some(pass)).await.unwrap();
    let second = register(server, Identity::generate(), room, owner_key, Some(pass)).await;
    assert!(matches!(second, Err(RendezvousError::Refused(Refusal::NotAllowed))));

    // The host closes the pass (it declined the joiner): the joiner is dropped from the room.
    owner.client.allow(Vec::new(), Vec::new()).await.unwrap();
    let gone = with_timeout(owner.client.introduce(joiner.identity.public())).await;
    assert!(matches!(gone, Err(RendezvousError::Timeout)), "{gone:?}");
    let back = register(server, Identity::generate(), room, owner_key, Some(pass)).await;
    assert!(matches!(back, Err(RendezvousError::Refused(Refusal::NotAllowed))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_for_one_call_refuses_other_rooms() {
    let owner = Identity::generate();
    let room = room_tag(&[3; 16]);
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node).only_room(room, owner.public());
    let addr = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());

    let other = Identity::generate();
    let other_key = other.public();
    let refused = register(addr, other, room_tag(&[4; 16]), other_key, None).await;
    assert!(matches!(refused, Err(RendezvousError::Refused(Refusal::UnknownRoom))));
    let squatter = Identity::generate();
    let squatter_key = squatter.public();
    let refused = register(addr, squatter, room, squatter_key, None).await;
    assert!(matches!(refused, Err(RendezvousError::Refused(Refusal::UnknownRoom))));
    let owner_key = owner.public();
    assert!(register(addr, owner, room, owner_key, None).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_for_chosen_hosts_refuses_strangers_but_not_their_guests() {
    let friend = Identity::generate();
    let friend_key = friend.public();
    let node = Node::bind(&Identity::generate(), localhost()).unwrap();
    let server = RendezvousServer::new(node).only_hosts([friend_key]);
    let addr = (server.local_addr().unwrap(), server.id());
    tokio::spawn(server.run());

    // A stranger can neither open a room nor ask for a reachability check.
    let stranger = Identity::generate();
    let stranger_key = stranger.public();
    let room = room_tag(&[5; 16]);
    let refused = register(addr, stranger, room, stranger_key, None).await;
    assert!(matches!(refused, Err(RendezvousError::Refused(Refusal::NotAllowed))));
    let prober = Node::bind(&Identity::generate(), localhost()).unwrap();
    let node_addr = veil_net::NodeAddr { key: addr.1, addr: addr.0 };
    let check = with_timeout(veil_rendezvous::check_reach(&prober, &node_addr, 9)).await;
    assert!(matches!(check, Err(RendezvousError::Refused(Refusal::NotAllowed))), "{check:?}");

    // The chosen host opens its room, and a guest it allows joins and gets a relay session.
    let mut owner = register(addr, friend, room, friend_key, None).await.unwrap();
    let mut visitor = guest(addr, room, &mut owner).await;
    let visitor_key = visitor.identity.public();
    let (asked, ()) = tokio::join!(
        with_timeout(visitor.client.open_relay(friend_key)),
        consent(&mut owner, visitor_key, Ask::Relay),
    );
    assert!(asked.is_ok(), "{asked:?}");
}