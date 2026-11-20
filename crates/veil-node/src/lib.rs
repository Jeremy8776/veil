//! `veil-node`: one deployable that fills any infrastructure role.
//!
//! Purpose: the same code runs on a participant's spare machine, a home server or an
//! enterprise cluster. The configuration file turns roles on: rendezvous (address reflection
//! and introductions, plus relay unless turned off) and forwarder. Each role listens on its own
//! port with the node's one device key.
//!
//! Invariants:
//! - A node never holds media or group keys. It carries ciphertext only, so choosing where
//!   nodes run is a capacity decision, never a security one.
//! - No function here panics. Startup failures are a [`NodeError`] with the file or role named.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod config;
pub mod identity;

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use tokio::task::JoinHandle;
use veil_forward::ForwarderServer;
use veil_net::{Node, NodeAddr};
use veil_rendezvous::RendezvousServer;
use veil_security::{Identity, PublicId};

pub use config::Config;

#[derive(Debug)]
pub enum NodeError {
    /// The configuration is not valid. The text says what to fix.
    Config(String),
    /// A file could not be read or written.
    Io(PathBuf, std::io::Error),
    /// A role could not bind its port.
    Bind(&'static str, veil_net::NetError),
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(why) => write!(f, "configuration: {why}"),
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::Bind(role, e) => write!(f, "{role} role could not start: {e}"),
        }
    }
}

impl std::error::Error for NodeError {}

/// A started node. Dropping it stops every role.
pub struct Running {
    pub id: PublicId,
    /// Where the rendezvous role listens, if it is on.
    pub rendezvous: Option<NodeAddr>,
    /// Where the forwarder role listens, if it is on.
    pub forwarder: Option<NodeAddr>,
    /// How many device keys may host here, or `None` when anyone may.
    pub hosts: Option<usize>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn bind(identity: &Identity, role: &'static str, listen: SocketAddr) -> Result<(Node, NodeAddr), NodeError> {
    let node = Node::bind(identity, listen).map_err(|e| NodeError::Bind(role, e))?;
    let addr = node.local_addr().map_err(|e| NodeError::Bind(role, e))?;
    Ok((node, NodeAddr { key: identity.public(), addr }))
}

/// Starts every role the configuration turns on. Must be called inside a Tokio runtime.
pub fn start(config: &Config, identity: &Identity) -> Result<Running, NodeError> {
    let mut running = Running {
        id: identity.public(),
        rendezvous: None,
        forwarder: None,
        hosts: None,
        tasks: Vec::new(),
    };
    let hosts = config.host_keys()?;
    running.hosts = hosts.as_ref().map(Vec::len);
    if let Some(role) = &config.rendezvous {
        let (node, addr) = bind(identity, "rendezvous", role.listen)?;
        let mut server = RendezvousServer::new(node);
        if !role.relay {
            server = server.without_relay();
        }
        if let Some(hosts) = &hosts {
            server = server.only_hosts(hosts.iter().copied());
        }
        running.tasks.push(tokio::spawn(server.run()));
        running.rendezvous = Some(addr);
    }
    if let Some(role) = &config.forwarder {
        let (node, addr) = bind(identity, "forwarder", role.listen)?;
        let mut server = ForwarderServer::new(node, role.limits());
        if let Some(hosts) = &hosts {
            server = server.only_hosts(hosts.iter().copied());
        }
        running.tasks.push(tokio::spawn(server.run()));
        running.forwarder = Some(addr);
    }
    Ok(running)
}
