//! Receiving video frames, several at once.
//!
//! Purpose: each sealed frame arrives on its own QUIC stream ([`Link::send_frame`]).
//! [`FrameReader`] reads up to [`MAX_FRAMES_IN_FLIGHT`] of them at the same time and returns
//! each as soon as it is complete, so a peer that never finishes one stream cannot hold back
//! the frames behind it.
//!
//! Invariants:
//! - A frame not complete within [`FRAME_DEADLINE`] is dropped and its stream stopped: a
//!   video frame that late is no use to the picture.
//! - Memory grows only as bytes arrive, never from the length a peer claims, and a stream
//!   that sends more or less than its header said is dropped.
//! - While `MAX_FRAMES_IN_FLIGHT` frames are being read, no new stream is accepted, so QUIC's
//!   stream limit pushes back on the sender.
//! - One bad stream never ends the reader. Only the link closing does.

use std::time::Duration;

use tokio::task::JoinSet;

use crate::endpoint::{Link, MAX_SEALED_FRAME};
use crate::NetError;

/// Frames read at the same time on one link.
pub const MAX_FRAMES_IN_FLIGHT: usize = 16;
/// How long one frame may take to arrive once its stream opens.
pub const FRAME_DEADLINE: Duration = Duration::from_secs(2);

pub struct FrameReader {
    conn: quinn::Connection,
    reading: JoinSet<Option<(u32, Vec<u8>)>>,
    closed: Option<String>,
}

impl FrameReader {
    pub fn new(link: &Link) -> Self {
        Self {
            conn: link.quic().clone(),
            reading: JoinSet::new(),
            closed: None,
        }
    }

    /// The next complete frame as `(frame_id, sealed)`, in the order frames complete.
    /// Fails only once the link has closed and every frame already started is done.
    pub async fn recv(&mut self) -> Result<(u32, Vec<u8>), NetError> {
        loop {
            let room = self.closed.is_none() && self.reading.len() < MAX_FRAMES_IN_FLIGHT;
            tokio::select! {
                accepted = self.conn.accept_uni(), if room => match accepted {
                    Ok(stream) => {
                        self.reading.spawn(read_frame(stream));
                    }
                    Err(e) => self.closed = Some(e.to_string()),
                },
                Some(done) = self.reading.join_next(), if !self.reading.is_empty() => {
                    if let Ok(Some(frame)) = done {
                        return Ok(frame);
                    }
                }
                else => {
                    let reason = self.closed.clone().unwrap_or_default();
                    return Err(NetError::Closed(reason));
                }
            }
        }
    }
}

/// Reads one frame stream, or `None` if it breaks the format or misses the deadline.
/// Dropping the stream early tells the sender to stop sending it.
async fn read_frame(stream: quinn::RecvStream) -> Option<(u32, Vec<u8>)> {
    tokio::time::timeout(FRAME_DEADLINE, read_whole(stream))
        .await
        .ok()
        .flatten()
}

pub(crate) async fn read_whole(mut stream: quinn::RecvStream) -> Option<(u32, Vec<u8>)> {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head).await.ok()?;
    let frame_id = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let len = usize::try_from(u32::from_be_bytes([head[4], head[5], head[6], head[7]])).ok()?;
    if len > MAX_SEALED_FRAME {
        return None;
    }
    // Grows with the bytes that arrive; fails if the stream sends more than `len`.
    let sealed = stream.read_to_end(len).await.ok()?;
    (sealed.len() == len).then_some((frame_id, sealed))
}
