#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use crate::NodeKind;

const MBPS: u64 = 1_000;

fn peer(id: u32, up_mbps: u64, down_mbps: u64, can_forward: bool) -> Node {
    Node {
        id: NodeId(id),
        kind: NodeKind::Peer,
        uplink_kbps: up_mbps * MBPS,
        downlink_kbps: down_mbps * MBPS,
        participant: true,
        can_forward,
    }
}

fn server(id: u32, kind: NodeKind, mbps: u64) -> Node {
    Node {
        id: NodeId(id),
        kind,
        uplink_kbps: mbps * MBPS,
        downlink_kbps: mbps * MBPS,
        participant: false,
        can_forward: true,
    }
}

/// Everyone watches `speaker` at Uhd and everyone else as a thumbnail.
fn grid(n: u32, speaker: u32) -> Vec<Subscription> {
    let mut subs = Vec::new();
    for r in 0..n {
        for s in (0..n).filter(|s| *s != r) {
            let layer = if s == speaker { Layer::Uhd } else { Layer::Thumb };
            subs.push(Subscription {
                receiver: NodeId(r),
                sender: NodeId(s),
                layer,
            });
        }
    }
    subs
}

fn everyone_at(n: u32, layer: Layer) -> Vec<Subscription> {
    let mut subs = grid(n, 0);
    for s in &mut subs {
        s.layer = layer;
    }
    subs
}

#[test]
fn two_people_on_fibre_get_4k_mesh() {
    let nodes = [peer(0, 100, 100, true), peer(1, 100, 100, true)];
    let plan = plan(&nodes, &everyone_at(2, Layer::Uhd), &Policy::default()).unwrap();
    assert!(matches!(plan, Plan::Mesh { degraded: false, .. }));
}

#[test]
fn mesh_is_preferred_over_a_server_when_it_fits() {
    let nodes = [
        peer(0, 100, 100, false),
        peer(1, 100, 100, false),
        server(9, NodeKind::Enterprise, 10_000),
    ];
    let plan = plan(&nodes, &everyone_at(2, Layer::Uhd), &Policy::default()).unwrap();
    assert!(matches!(plan, Plan::Mesh { .. }));
}

#[test]
fn slow_uploads_degrade_instead_of_failing() {
    let nodes = [
        peer(0, 10, 100, false),
        peer(1, 10, 100, false),
        peer(2, 10, 100, false),
    ];
    let plan = plan(&nodes, &everyone_at(3, Layer::Uhd), &Policy::default()).unwrap();
    match plan {
        Plan::Mesh { subscriptions, degraded } => {
            assert!(degraded);
            assert!(subscriptions.iter().all(|s| s.layer == Layer::Hd));
        }
        other => panic!("expected degraded mesh, got {other:?}"),
    }
}

#[test]
fn sixteen_people_use_one_enterprise_forwarder() {
    let mut nodes: Vec<Node> = (0..16).map(|i| peer(i, 100, 100, true)).collect();
    nodes.push(server(100, NodeKind::Enterprise, 10_000));
    let plan = plan(&nodes, &grid(16, 0), &Policy::default()).unwrap();
    match plan {
        Plan::Forwarded { forwarders, home, degraded, .. } => {
            assert_eq!(forwarders, vec![NodeId(100)]);
            assert_eq!(home.len(), 16);
            assert!(!degraded);
        }
        other => panic!("expected forwarded, got {other:?}"),
    }
}

#[test]
fn a_fibre_participant_can_host_sixteen_people() {
    let nodes: Vec<Node> = (0..16).map(|i| peer(i, 1_000, 1_000, true)).collect();
    let plan = plan(&nodes, &grid(16, 0), &Policy::default()).unwrap();
    match plan {
        Plan::Forwarded { forwarders, home, degraded, .. } => {
            assert_eq!(forwarders.len(), 1);
            assert_eq!(home.get(&forwarders[0]), Some(&forwarders[0]));
            assert!(!degraded);
        }
        other => panic!("expected forwarded, got {other:?}"),
    }
}

#[test]
fn small_servers_cascade() {
    let mut nodes: Vec<Node> = (0..16).map(|i| peer(i, 100, 100, false)).collect();
    for id in 100..104 {
        nodes.push(server(id, NodeKind::SelfHosted, 300));
    }
    let plan = plan(&nodes, &grid(16, 0), &Policy::default()).unwrap();
    match plan {
        Plan::Forwarded { forwarders, degraded, .. } => {
            assert!(forwarders.len() >= 2, "got {forwarders:?}");
            assert!(!degraded);
        }
        other => panic!("expected cascade, got {other:?}"),
    }
}

#[test]
fn policy_order_puts_enterprise_first() {
    let mut nodes: Vec<Node> = (0..8).map(|i| peer(i, 1_000, 1_000, true)).collect();
    nodes.push(server(100, NodeKind::Enterprise, 1_000));
    let plan = plan(&nodes, &grid(8, 0), &Policy::default()).unwrap();
    match plan {
        Plan::Forwarded { forwarders, .. } => assert_eq!(forwarders[0], NodeId(100)),
        other => panic!("expected forwarded, got {other:?}"),
    }
}

#[test]
fn our_servers_only_policy_never_uses_a_peer() {
    let nodes: Vec<Node> = (0..8).map(|i| peer(i, 1_000, 1_000, true)).collect();
    let policy = Policy {
        forwarder_preference: vec![NodeKind::Enterprise],
        ..Policy::default()
    };
    assert_eq!(plan(&nodes, &grid(8, 0), &policy), Err(PlanError::Infeasible));
}

#[test]
fn invalid_input_is_refused() {
    let nodes = [peer(0, 100, 100, true), server(9, NodeKind::Enterprise, 100)];
    let me = Subscription { receiver: NodeId(0), sender: NodeId(0), layer: Layer::Hd };
    assert_eq!(
        plan(&nodes, &[me], &Policy::default()),
        Err(PlanError::SelfSubscription(NodeId(0)))
    );
    let ghost = Subscription { receiver: NodeId(0), sender: NodeId(5), layer: Layer::Hd };
    assert_eq!(
        plan(&nodes, &[ghost], &Policy::default()),
        Err(PlanError::UnknownNode(NodeId(5)))
    );
    let to_server = Subscription { receiver: NodeId(0), sender: NodeId(9), layer: Layer::Hd };
    assert_eq!(
        plan(&nodes, &[to_server], &Policy::default()),
        Err(PlanError::NotParticipant(NodeId(9)))
    );
    let twice = [peer(0, 1, 1, true), peer(0, 1, 1, true)];
    assert_eq!(
        plan(&twice, &[], &Policy::default()),
        Err(PlanError::DuplicateNode(NodeId(0)))
    );
}

#[test]
fn same_input_gives_same_plan() {
    let nodes: Vec<Node> = (0..10).map(|i| peer(i, 500, 500, true)).collect();
    let a = plan(&nodes, &grid(10, 3), &Policy::default()).unwrap();
    let b = plan(&nodes, &grid(10, 3), &Policy::default()).unwrap();
    assert_eq!(a, b);
}
