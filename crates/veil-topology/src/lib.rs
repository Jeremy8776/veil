//! Call topology planner.
//!
//! Purpose: given who is in a call, what each link can carry, which nodes may forward and what
//! each receiver wants to watch, choose the cheapest layout that fits:
//!
//! 1. **Mesh**: everyone sends to everyone. Lowest latency. Tried first when the call is small.
//! 2. **Forwarded**: everyone uploads once to a forwarder. One or more forwarders (a cascade)
//!    split the receivers between them and exchange each stream once.
//! 3. **Degraded**: when nothing fits, lower the highest layer in use and try again.
//!
//! A forwarder can be a participant, a self-hosted node or an enterprise node. They are the same
//! role; [`Policy`] decides which kinds are allowed and in what order. Forwarders carry
//! ciphertext only, so this choice is about capacity, never about trust.
//!
//! Invariants: pure and deterministic (same input, same plan); every loop is bounded; no
//! panics; arithmetic saturates.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod plan;

pub use plan::plan;

use std::collections::BTreeMap;
use std::fmt;

/// Opus audio, with packet overhead, per stream.
pub const AUDIO_KBPS: u64 = 48;
/// Largest call the planner accepts.
pub const MAX_NODES: usize = 1_024;

/// Video quality layers. Each sender encodes all requested layers once (SVC or simulcast).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// 360p15, about 0.5 Mbps.
    Thumb,
    /// 1080p30, about 3 Mbps.
    Hd,
    /// 2160p60, about 20 Mbps.
    Uhd,
}

impl Layer {
    pub const fn kbps(self) -> u64 {
        match self {
            Self::Thumb => 500,
            Self::Hd => 3_000,
            Self::Uhd => 20_000,
        }
    }

    const fn lower(self) -> Self {
        match self {
            Self::Uhd => Self::Hd,
            Self::Hd | Self::Thumb => Self::Thumb,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Where a node runs. Same software and protocol in every case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A participant's own device.
    Peer,
    /// A `veil-node` someone runs themselves.
    SelfHosted,
    /// A `veil-node` in an organisation's own infrastructure.
    Enterprise,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Measured capacity, in kilobits per second.
    pub uplink_kbps: u64,
    pub downlink_kbps: u64,
    /// Sends and receives media. A dedicated server node is not a participant.
    pub participant: bool,
    /// Willing and able to forward for others.
    pub can_forward: bool,
}

/// `receiver` wants to watch `sender` at `layer`. Audio is implied for every pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub receiver: NodeId,
    pub sender: NodeId,
    pub layer: Layer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Allowed forwarder kinds, most preferred first. Leave a kind out to forbid it.
    pub forwarder_preference: Vec<NodeKind>,
    /// Share of measured capacity the plan may use, 1 to 100.
    pub headroom_percent: u64,
    /// Largest call that may use a mesh.
    pub mesh_max: usize,
    /// Most forwarders one call may cascade across.
    pub max_forwarders: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            forwarder_preference: vec![NodeKind::Enterprise, NodeKind::SelfHosted, NodeKind::Peer],
            headroom_percent: 80,
            mesh_max: 4,
            max_forwarders: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Mesh {
        subscriptions: Vec<Subscription>,
        degraded: bool,
    },
    Forwarded {
        /// Forwarders in use, most preferred first.
        forwarders: Vec<NodeId>,
        /// Which forwarder each participant connects to.
        home: BTreeMap<NodeId, NodeId>,
        subscriptions: Vec<Subscription>,
        degraded: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    DuplicateNode(NodeId),
    UnknownNode(NodeId),
    NotParticipant(NodeId),
    SelfSubscription(NodeId),
    TooManyNodes,
    /// Nothing fits, even with every stream at the lowest layer.
    Infeasible,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateNode(id) => write!(f, "node {} is listed twice", id.0),
            Self::UnknownNode(id) => write!(f, "node {} is not in the call", id.0),
            Self::NotParticipant(id) => write!(f, "node {} is not a participant", id.0),
            Self::SelfSubscription(id) => write!(f, "node {} subscribed to itself", id.0),
            Self::TooManyNodes => write!(f, "more than {MAX_NODES} nodes"),
            Self::Infeasible => f.write_str("no layout fits the available bandwidth"),
        }
    }
}

impl std::error::Error for PlanError {}
