//! Routes each incoming link by its first control message.
//!
//! A participant's node accepts two kinds of link: joiners knocking on the host (`Hello`) and
//! other members opening media (`Media`). One accept loop per node reads the first message and
//! hands the link on. Relayed links arrive on a virtual node and are routed the same way.
//!
//! Invariants:
//! - A link that does not send a known first message in [`FIRST_MESSAGE`] is closed.
//! - `Hello` goes to the host session only if this participant hosts; otherwise it is closed.
//! - `Media` for another room is closed. Whether the key is a member is checked by the media
//!   session, which knows the roster.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use veil_net::control::Control;
use veil_net::{FramedRecv, FramedSend, Link, Node};

use crate::host::HostHandle;

const FIRST_MESSAGE: Duration = Duration::from_secs(10);

/// An accepted link and its opened control stream.
pub(crate) struct Inbound {
    pub link: Link,
    pub out: FramedSend,
    pub input: FramedRecv,
    pub first: Control,
    /// The virtual node a relayed link runs on. Held so the link outlives its router.
    pub via: Option<Arc<Node>>,
}

#[derive(Clone)]
pub(crate) struct Router {
    pub room_id: [u8; 16],
    pub host: Option<HostHandle>,
    pub media: mpsc::Sender<Inbound>,
}

impl Router {
    /// Accepts links on `node` until it closes. `relayed` marks a virtual node, which every
    /// accepted link then keeps alive.
    pub async fn accept_loop(self, node: Arc<Node>, relayed: bool) {
        while let Some(incoming) = node.accept().await {
            if let Ok(link) = incoming {
                let via = relayed.then(|| node.clone());
                tokio::spawn(self.clone().route(link, via));
            }
        }
    }

    async fn route(self, link: Link, via: Option<Arc<Node>>) {
        let opened = tokio::time::timeout(FIRST_MESSAGE, async {
            let (out, mut input) = link.accept_control().await.ok()?.split();
            let first = input.recv::<Control>().await.ok()?;
            Some((out, input, first))
        })
        .await;
        let Ok(Some((out, input, first))) = opened else {
            link.close("no first message");
            return;
        };
        let inbound = Inbound {
            link,
            out,
            input,
            first,
            via,
        };
        match &inbound.first {
            Control::Hello { .. } => {
                if let Some(host) = &self.host {
                    let _ = host.add_inbound(inbound).await;
                } else {
                    inbound.link.close("not hosting");
                }
            }
            Control::Media { room_id } if *room_id == self.room_id => {
                if let Err(e) = self.media.try_send(inbound) {
                    let inbound = match e {
                        mpsc::error::TrySendError::Full(i) | mpsc::error::TrySendError::Closed(i) => i,
                    };
                    inbound.link.close("busy");
                }
            }
            _ => inbound.link.close("unexpected first message"),
        }
    }
}
