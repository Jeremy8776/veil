//! Routes each incoming link by its first control message.
//!
//! A participant's node accepts two kinds of link: joiners knocking on the host (`Hello`) and
//! other members opening media (`Media`). One accept loop per node reads the first message and
//! hands the link on. Relayed links arrive on a virtual node and are routed the same way.
//!
//! Invariants:
//! - A relay session carries one link. Its virtual node takes the first link within the wait
//!   it is given and no other. The link then keeps the node; with no link the node is dropped.
//! - A link that does not send a known first message in [`FIRST_MESSAGE`] is closed.
//! - `Hello` goes to the host session only if this participant hosts; otherwise it is closed.
//! - `Media` for another room is closed. Whether the key is a member is checked by the media
//!   session, which knows the roster.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
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
    /// The host session, while this participant hosts. Set later when it takes over.
    pub host: watch::Receiver<Option<HostHandle>>,
    pub media: mpsc::Sender<Inbound>,
}

impl Router {
    /// Accepts links on `node` until it closes.
    pub async fn accept_loop(self, node: Arc<Node>) {
        while let Some(incoming) = node.accept().await {
            if let Ok(link) = incoming {
                tokio::spawn(self.clone().route(link, None));
            }
        }
    }

    /// Waits up to `wait` for the one link of a relay session on its virtual `node`, routes
    /// it and ends. The link keeps `node` alive.
    pub async fn accept_relayed(self, node: Arc<Node>, wait: Duration) {
        let Ok(Some(Ok(link))) = tokio::time::timeout(wait, node.accept()).await else {
            return;
        };
        node.stop_accepting().await;
        tokio::spawn(self.route(link, Some(node)));
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
            Control::Hello { .. } | Control::Resume { .. } => {
                let host = self.host.borrow().clone();
                if let Some(host) = host {
                    let _ = host.add_inbound(inbound).await;
                } else {
                    inbound.link.close("not hosting");
                }
            }
            Control::Media { room_id } if *room_id == self.room_id => {
                if let Err(e) = self.media.try_send(inbound) {
                    let inbound = match e {
                        mpsc::error::TrySendError::Full(i)
                        | mpsc::error::TrySendError::Closed(i) => i,
                    };
                    inbound.link.close("busy");
                }
            }
            _ => inbound.link.close("unexpected first message"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use veil_security::Identity;

    use super::*;

    const ROOM: [u8; 16] = [3; 16];
    const SOON: Duration = Duration::from_secs(5);

    fn bind(identity: &Identity) -> Node {
        Node::bind(identity, "127.0.0.1:0".parse().unwrap()).unwrap()
    }

    fn router() -> (Router, mpsc::Receiver<Inbound>) {
        let (media, inbox) = mpsc::channel(4);
        (
            Router {
                room_id: ROOM,
                host: watch::channel(None).1,
                media,
            },
            inbox,
        )
    }

    #[tokio::test]
    async fn a_session_with_no_link_lets_go_of_its_node() {
        let node = Arc::new(bind(&Identity::generate()));
        let (router, _inbox) = router();
        let wait = router.accept_relayed(node.clone(), Duration::from_millis(20));
        tokio::time::timeout(SOON, wait)
            .await
            .expect("the wait ends by itself");
        assert_eq!(Arc::strong_count(&node), 1);
    }

    #[tokio::test]
    async fn a_session_routes_one_link_keeps_it_and_takes_no_other() {
        let identity = Identity::generate();
        let node = Arc::new(bind(&identity));
        let (addr, key) = (node.local_addr().unwrap(), identity.public());
        let (router, mut inbox) = router();
        let waiting = tokio::spawn(router.accept_relayed(node.clone(), SOON));
        drop(node);

        let dialler = bind(&Identity::generate());
        let link = dialler.connect(&[addr], key).await.unwrap();
        let (mut out, _input) = link.open_control().await.unwrap().split();
        out.send(&Control::Media { room_id: ROOM }).await.unwrap();
        tokio::time::timeout(SOON, waiting).await.unwrap().unwrap();

        let inbound = tokio::time::timeout(SOON, inbox.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(inbound.via.is_some(), "the link holds its node");
        assert_eq!(inbound.link.peer(), dialler.id());

        let second =
            tokio::time::timeout(Duration::from_secs(1), dialler.connect(&[addr], key)).await;
        assert!(!matches!(second, Ok(Ok(_))), "a session carries one link");
    }
}
