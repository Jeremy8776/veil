//! Sending on one media link without letting that link's peer hold the sender.
//!
//! Purpose: QUIC flow control is set by the receiving end. A member whose client grants no
//! stream credit, or stops reading, would otherwise keep any send to it pending for as long as
//! it likes, and with it the caller's whole call loop (GHSA-37f5-22p2-q6hr).
//!
//! Invariants:
//! - Video goes through one writer task per link with a queue of [`FRAME_QUEUE`] frames.
//!   `MediaSession::send_frame` only queues, and drops the frame for a link whose queue is
//!   full, so no peer can hold it.
//! - Every send on a link (a video frame, a control message, opening the control stream) has
//!   [`SEND_DEADLINE`]. A control message that misses it closes the link: control messages are
//!   tiny, so an honest peer takes them at once. Video closes the link after [`MAX_STALLS`]
//!   frames in a row miss it, so one slow keyframe on a poor link does not end the call.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use veil_net::control::Control;

use super::Carrier;
use crate::CallError;

/// The longest any one send on a media link may take.
pub const SEND_DEADLINE: Duration = Duration::from_secs(5);
/// Video frames waiting for one link.
const FRAME_QUEUE: usize = 8;
/// Video frames in a row that may miss the deadline before the link is closed.
const MAX_STALLS: u32 = 3;

/// A queued video frame: its id and the sealed bytes, shared between links.
pub(super) type Frame = (u32, Arc<[u8]>);

/// Starts the writer task for a link. Frames sent to the returned queue go out in order.
pub(super) fn spawn_writer(carrier: Arc<Carrier>) -> (mpsc::Sender<Frame>, JoinHandle<()>) {
    let (frames, queue) = mpsc::channel(FRAME_QUEUE);
    (frames, tokio::spawn(writer(carrier, queue)))
}

async fn writer(carrier: Arc<Carrier>, mut queue: mpsc::Receiver<Frame>) {
    let mut stalls = 0;
    while let Some((frame_id, sealed)) = queue.recv().await {
        match timeout(SEND_DEADLINE, carrier.link().send_frame(frame_id, &sealed)).await {
            Ok(_) => stalls = 0,
            Err(_) => {
                stalls += 1;
                if stalls >= MAX_STALLS {
                    carrier.link().close("peer stopped taking video");
                    return;
                }
            }
        }
    }
}

/// Sends one message on a member link's control stream, or closes the link if the peer does
/// not take it within [`SEND_DEADLINE`].
pub(super) async fn control(carrier: &Carrier, message: &Control) -> Result<(), CallError> {
    let Carrier::Member { link, control } = carrier else {
        return Err(CallError::Protocol("not a member link"));
    };
    let send = async { control.lock().await.send(message).await };
    match timeout(SEND_DEADLINE, send).await {
        Ok(result) => Ok(result?),
        Err(_) => {
            link.close("peer stopped taking control messages");
            Err(CallError::Stalled)
        }
    }
}

/// Runs a send towards a forwarder with [`SEND_DEADLINE`].
pub(super) async fn forwarder<T>(
    send: impl Future<Output = Result<T, veil_forward::ForwardError>>,
) -> Result<T, CallError> {
    match timeout(SEND_DEADLINE, send).await {
        Ok(result) => result.map_err(CallError::Forward),
        Err(_) => Err(CallError::Stalled),
    }
}
