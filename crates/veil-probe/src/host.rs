//! `veil-probe host`: starts a call, admits every joiner, plans each epoch and probes.
//!
//! Every knock is admitted: the invite's one-time token is the only gate, which is enough for
//! a test between your own machines. A fresh invite is printed after each join.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{MissedTickBehavior, interval};
use veil_call::{CallNode, Directory, HostEvent, HostHandle, Peer};
use veil_security::invite::MAX_ADDRS;
use veil_security::safety::safety_number;
use veil_security::{Identity, PublicId};
use veil_topology::{NodeId, NodeKind, Plan, Policy, plan};

use crate::args::HostArgs;
use crate::session::{Session, short};

/// Planner id for the forwarder; member ids are their leaf indexes, far below this.
const FORWARDER_ID: NodeId = NodeId(u32::MAX);
/// The planner needs capacities. Real measurement belongs to `veil-media`; until then the probe
/// claims generous links so the plan follows the policy alone.
const ASSUMED_KBPS: u64 = 100_000;
const INVITE_LIFETIME: Duration = Duration::from_secs(3_600);

pub async fn run(mut args: HostArgs) -> Result<(), String> {
    let identity = Identity::generate();
    // A fresh key's public half is 32 random-looking bytes, so its first 16 make a room id
    // without adding a random number dependency.
    let room_id: [u8; 16] = Identity::generate().public().0[..16]
        .try_into()
        .map_err(|_| "room id")?;
    let node = crate::bind(&identity, args.bind)?;
    // A public IPv6 address joins the invite by itself, so two IPv6 machines link over it.
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    if let Some(ip) = node.ipv6().then(veil_net::family::global_ipv6).flatten() {
        let addr = std::net::SocketAddr::new(ip.into(), port);
        println!("public IPv6 address {addr} goes in the invite");
        if !args.advertise.contains(&addr) && args.advertise.len() < MAX_ADDRS {
            args.advertise.push(addr);
        }
    }
    let (host, mut events) = HostHandle::start(&identity, room_id).map_err(|e| e.to_string())?;
    let mut call = CallNode::start(
        &identity,
        node,
        room_id,
        args.rendezvous,
        host.clone(),
    )
    .await;
    println!(
        "host {} listening on {}",
        short(&identity.public()),
        call.local_addr().map_err(|e| e.to_string())?
    );
    match (args.rendezvous, call.has_rendezvous()) {
        (Some(r), true) => println!("registered with rendezvous {r}"),
        (Some(r), false) => println!("WARNING: rendezvous {r} did not answer; direct links only"),
        (None, _) => {}
    }
    if args.advertise.is_empty() && args.rendezvous.is_none() {
        println!("WARNING: no --advertise and no --rendezvous: joiners cannot reach this host");
    }
    let mut session = Session::new(call.media().map_err(|e| e.to_string())?, identity.public());
    print_invite(&host, &args).await?;

    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut report = interval(Duration::from_secs(10));
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(HostEvent::Knock(device)) => {
                    println!("knock from {}, safety number {}", short(&device), safety_number(&identity.public(), &device));
                    host.decide(device, true).await.map_err(|e| e.to_string())?;
                }
                Some(HostEvent::Joined { device, leaf }) => {
                    println!("{} joined as member {leaf}", short(&device));
                    print_invite(&host, &args).await?;
                }
                Some(HostEvent::Left(device)) => println!("{} left", short(&device)),
                Some(HostEvent::Keys(keys)) => {
                    let roster = keys.roster.clone();
                    session.on_keys(keys);
                    if roster.len() > 1 {
                        let (plan, directory) = plan_for(&roster, identity.public(), &args)?;
                        match host.publish_plan(plan, directory).await {
                            Ok(routes) => session.on_routes(routes).await,
                            Err(e) => println!("could not publish the plan: {e}"),
                        }
                    }
                }
                Some(HostEvent::Error(e)) => println!("group error: {e}"),
                None => return Err("the host session stopped".into()),
            },
            Some(incoming) = session.media.recv() => session.on_incoming(incoming).await,
            _ = tick.tick() => session.ping().await,
            _ = report.tick() => session.report().iter().for_each(|line| println!("{line}")),
            _ = &mut stop => return Ok(()),
        }
    }
}

async fn print_invite(host: &HostHandle, args: &HostArgs) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let addrs = args.advertise.iter().map(ToString::to_string).collect();
    let relay = args.rendezvous.map(|r| r.to_string());
    let link = host
        .invite(addrs, relay, now + INVITE_LIFETIME.as_secs())
        .await
        .map_err(|e| e.to_string())?;
    println!("invite (one use, 1 hour):\n{link}");
    Ok(())
}

/// This epoch's plan and the directory behind it. With a forwarder every member goes through
/// it; without one the call is a mesh. Only the host's own addresses are known, so members
/// reach each other by punching through the rendezvous.
pub fn plan_for(
    roster: &[(u32, PublicId)],
    me: PublicId,
    args: &HostArgs,
) -> Result<(Plan, Directory), String> {
    let mine = roster
        .iter()
        .find(|(_, d)| *d == me)
        .ok_or("the host is not in its own roster")?;
    let mut nodes: Vec<veil_topology::Node> = roster
        .iter()
        .map(|(leaf, _)| veil_topology::Node {
            id: NodeId(*leaf),
            kind: NodeKind::Peer,
            uplink_kbps: ASSUMED_KBPS,
            downlink_kbps: ASSUMED_KBPS,
            participant: true,
            can_forward: false,
        })
        .collect();
    let mut policy = Policy::default();
    if args.forwarder.is_some() {
        nodes.push(veil_topology::Node {
            id: FORWARDER_ID,
            kind: NodeKind::SelfHosted,
            uplink_kbps: ASSUMED_KBPS * 10,
            downlink_kbps: ASSUMED_KBPS * 10,
            participant: false,
            can_forward: true,
        });
        policy.mesh_max = 1;
    }
    let plan = plan(&nodes, &[], &policy).map_err(|e| format!("cannot plan: {e:?}"))?;
    let directory = Directory {
        me: NodeId(mine.0),
        members: roster
            .iter()
            .map(|(leaf, device)| {
                let addrs = if *device == me {
                    args.advertise.clone()
                } else {
                    Vec::new()
                };
                (
                    NodeId(*leaf),
                    Peer {
                        device: *device,
                        addrs,
                    },
                )
            })
            .collect(),
        forwarders: args
            .forwarder
            .map(|f| (FORWARDER_ID, f))
            .into_iter()
            .collect(),
    };
    Ok((plan, directory))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_net::NodeAddr;

    use super::*;

    fn args(forwarder: Option<NodeAddr>) -> HostArgs {
        HostArgs {
            bind: "0.0.0.0:0".parse().unwrap(),
            advertise: vec!["198.51.100.2:7710".parse().unwrap()],
            rendezvous: None,
            forwarder,
        }
    }

    #[test]
    fn without_a_forwarder_the_call_is_a_mesh_with_the_hosts_address() {
        let (host, other) = (Identity::generate().public(), Identity::generate().public());
        let roster = [(0, host), (1, other)];
        let (plan, directory) = plan_for(&roster, host, &args(None)).unwrap();
        assert!(matches!(plan, Plan::Mesh { .. }), "{plan:?}");
        assert_eq!(directory.me, NodeId(0));
        assert_eq!(directory.members[&NodeId(0)].addrs.len(), 1);
        assert!(directory.members[&NodeId(1)].addrs.is_empty());
    }

    #[test]
    fn a_forwarder_carries_every_member() {
        let (host, other) = (Identity::generate().public(), Identity::generate().public());
        let forwarder = NodeAddr {
            key: Identity::generate().public(),
            addr: "203.0.113.5:7701".parse().unwrap(),
        };
        let roster = [(0, other), (1, host)];
        let (plan, directory) = plan_for(&roster, host, &args(Some(forwarder))).unwrap();
        assert!(!matches!(plan, Plan::Mesh { .. }), "{plan:?}");
        assert_eq!(directory.me, NodeId(1));
        assert_eq!(directory.forwarders[&FORWARDER_ID], forwarder);
    }

    #[test]
    fn a_roster_without_the_host_is_refused() {
        let roster = [(0, Identity::generate().public())];
        assert!(plan_for(&roster, Identity::generate().public(), &args(None)).is_err());
    }
}
