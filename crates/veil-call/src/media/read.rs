//! Reader tasks for one media link: video frames, audio datagrams, and control messages.
//!
//! Invariants:
//! - Video waits for room in the incoming queue; audio and keyframe requests never wait, so a
//!   full queue drops them. A member flooding requests can therefore not stall video.
//! - Only `KeyframeRequest` is read from the control stream after the link's first message.
//!   Anything else is ignored.

use std::sync::Arc;

use tokio::sync::mpsc;
use veil_forward::protocol::ServerMsg;
use veil_net::FramedRecv;
use veil_net::control::Control;
use veil_security::PublicId;

use super::{Carrier, Ctx, Incoming, Media};

pub(super) async fn video(carrier: Arc<Carrier>, via: PublicId, id: u64, ctx: Ctx) {
    while let Ok((frame_id, sealed)) = carrier.link().recv_frame().await {
        let media = Media::Video { frame_id, sealed };
        if ctx.incoming.send(Incoming { via, media }).await.is_err() {
            return;
        }
    }
    let _ = ctx.closed.send((via, id)).await;
}

pub(super) async fn audio(carrier: Arc<Carrier>, via: PublicId, incoming: mpsc::Sender<Incoming>) {
    while let Ok(packet) = carrier.link().recv_datagram().await {
        // Audio is late or lost, never queued: a full channel drops the packet.
        let _ = incoming.try_send(Incoming {
            via,
            media: Media::Audio(packet),
        });
    }
}

/// A member link's control stream.
pub(super) async fn member_control(mut input: FramedRecv, via: PublicId, incoming: mpsc::Sender<Incoming>) {
    while let Ok(message) = input.recv::<Control>().await {
        if message == Control::KeyframeRequest {
            let _ = incoming.try_send(Incoming {
                via,
                media: Media::KeyframeRequest,
            });
        }
    }
}

/// A forwarder link's control stream.
pub(super) async fn forwarder_control(carrier: Arc<Carrier>, via: PublicId, incoming: mpsc::Sender<Incoming>) {
    let Carrier::Forwarder(client) = &*carrier else {
        return;
    };
    while let Ok(message) = client.recv().await {
        if message == ServerMsg::KeyframeRequest {
            let _ = incoming.try_send(Incoming {
                via,
                media: Media::KeyframeRequest,
            });
        }
    }
}
