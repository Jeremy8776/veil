//! One link's tasks: a reader that forwards control messages to the actor, and a writer that
//! sends what the actor queues. Address hints are filtered here, where the link's observed
//! address is known.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use veil_net::control::Control;
use veil_net::{FramedRecv, FramedSend, Link, Node};

use super::hints::member_hints;
use super::{Internal, LINGER, OUTBOX};

/// A link whose control stream is already open and whose first message is already read.
pub(super) struct Opened {
    pub(super) out: FramedSend,
    pub(super) input: FramedRecv,
    pub(super) first: Control,
    /// Keeps a relayed link's virtual node alive for as long as the link.
    pub(super) _via: Option<Arc<Node>>,
}

pub(super) async fn run_link(link: Link, opened: Option<Opened>, id: u64, actor: mpsc::Sender<Internal>) {
    let device = link.peer();
    let (mut out, mut input, first, _via) = match opened {
        Some(o) => (o.out, o.input, Some(o.first), o._via),
        None => {
            let Ok(control) = link.accept_control().await else {
                return;
            };
            let (out, input) = control.split();
            (out, input, None, None)
        }
    };
    let first = match first {
        Some(first) => first,
        None => {
            match tokio::time::timeout(Duration::from_secs(10), input.recv::<Control>()).await {
                Ok(Ok(first)) => first,
                _ => {
                    link.close("no first message");
                    return;
                }
            }
        }
    };
    let (outbox, mut queue) = mpsc::channel(OUTBOX);
    if actor
        .send(Internal::Opened(device, id, outbox, first))
        .await
        .is_err()
    {
        return;
    }
    let link = Arc::new(link);
    let reader = {
        let actor = actor.clone();
        let link = link.clone();
        tokio::spawn(async move {
            while let Ok(message) = input.recv::<Control>().await {
                let message = match message {
                    Control::Addresses(addrs) => match member_hints(addrs, link.remote_addr()) {
                        Some(addrs) => Control::Addresses(addrs),
                        None => continue,
                    },
                    other => other,
                };
                if actor
                    .send(Internal::Message(device, id, message))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = actor
                .send(Internal::Closed(device, id, link.peer_closed_with("left")))
                .await;
        })
    };
    while let Some(message) = queue.recv().await {
        if out.send(&message).await.is_err() {
            // A device the host can no longer write to must not stay in the roster.
            let _ = actor
                .send(Internal::Closed(device, id, link.peer_closed_with("left")))
                .await;
            break;
        }
    }
    // The actor let go of this link. Let the last message arrive before closing.
    drop(out);
    let _ = tokio::time::timeout(LINGER, link.quic().closed()).await;
    link.close("bye");
    reader.abort();
}
