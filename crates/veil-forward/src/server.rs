//! The forwarding service.
//!
//! State lives behind one mutex that is never held across an `.await`. Each member link has a
//! control reader (which also detects the link closing), a video reader, an audio reader and a
//! video writer. Video for a member waits in a queue bounded in bytes; when the queue is full,
//! new frames for that member are dropped.
//!
//! shortcut: every member receives every stream at full quality. Upgrade to per-receiver layer
//! subscriptions once `veil-media` puts SVC layer markers in the plaintext frame header.
//!
//! shortcut: any device that knows the room tag can join, so a declined joiner could still
//! receive ciphertext and timing. Upgrade to host-signed roster tickets checked on `Join`.
//!
//! shortcut: no forwarder-to-forwarder cascade yet; one forwarder serves a whole room.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use tokio::sync::mpsc;
use veil_net::{Link, NetError, Node};
use veil_security::PublicId;

use crate::protocol::{ClientMsg, Refusal, ServerMsg};

/// Frames waiting for one receiver, counted in frames. The byte budget usually binds first.
const VIDEO_QUEUE: usize = 256;
const REFUSAL_LINGER: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_rooms: usize,
    pub max_members_per_room: usize,
    /// Video bytes allowed to wait for one receiver before frames are dropped.
    pub queue_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rooms: 1_000,
            max_members_per_room: 16,
            queue_bytes: 32 * 1024 * 1024,
        }
    }
}

type Frame = (u32, Arc<Vec<u8>>);

struct Member {
    video: mpsc::Sender<Frame>,
    queued: Arc<AtomicUsize>,
    conn: quinn::Connection,
}

#[derive(Default)]
struct State {
    rooms: HashMap<[u8; 32], HashMap<PublicId, Member>>,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> Option<MutexGuard<'_, State>> {
    // A poisoned lock means a handler panicked; stop serving rather than trust the state.
    state.lock().ok()
}

pub struct ForwarderServer {
    node: Node,
    state: Shared,
    limits: Limits,
}

impl ForwarderServer {
    pub fn new(node: Node, limits: Limits) -> Self {
        Self {
            node,
            state: Shared::default(),
            limits,
        }
    }

    pub fn id(&self) -> PublicId {
        self.node.id()
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.node.local_addr()
    }

    /// Serves until the node is closed.
    pub async fn run(self) {
        while let Some(incoming) = self.node.accept().await {
            if let Ok(link) = incoming {
                tokio::spawn(serve(link, self.state.clone(), self.limits));
            }
        }
    }
}

fn join(state: &Shared, limits: &Limits, tag: [u8; 32], me: PublicId, member: Member) -> Result<u16, Refusal> {
    let mut s = lock(state).ok_or(Refusal::Full)?;
    if !s.rooms.contains_key(&tag) && s.rooms.len() >= limits.max_rooms {
        return Err(Refusal::Full);
    }
    let room = s.rooms.entry(tag).or_default();
    if room.contains_key(&me) {
        return Err(Refusal::AlreadyJoined);
    }
    if room.len() >= limits.max_members_per_room {
        return Err(Refusal::RoomFull);
    }
    room.insert(me, member);
    Ok(u16::try_from(room.len()).unwrap_or(u16::MAX))
}

fn leave(state: &Shared, tag: [u8; 32], me: PublicId) {
    if let Some(mut s) = lock(state) {
        if let Some(room) = s.rooms.get_mut(&tag) {
            room.remove(&me);
            if room.is_empty() {
                s.rooms.remove(&tag);
            }
        }
    }
}

/// Everyone in the room except the sender.
fn others<T>(state: &Shared, tag: &[u8; 32], me: PublicId, pick: impl Fn(&Member) -> T) -> Vec<T> {
    lock(state)
        .and_then(|s| {
            s.rooms.get(tag).map(|room| {
                room.iter()
                    .filter(|(key, _)| **key != me)
                    .map(|(_, m)| pick(m))
                    .collect()
            })
        })
        .unwrap_or_default()
}

async fn serve(link: Link, state: Shared, limits: Limits) {
    let link = Arc::new(link);
    let Ok(control) = link.accept_control().await else {
        return;
    };
    let (mut out, mut input) = control.split();
    let Ok(ClientMsg::Join { room_tag: tag }) = input.recv::<ClientMsg>().await else {
        return;
    };
    let me = link.peer();
    let (video_tx, mut video_rx) = mpsc::channel::<Frame>(VIDEO_QUEUE);
    let queued = Arc::new(AtomicUsize::new(0));
    let member = Member {
        video: video_tx,
        queued: queued.clone(),
        conn: link.quic().clone(),
    };
    match join(&state, &limits, tag, me, member) {
        Ok(members) => {
            if out.send(&ServerMsg::Joined { members }).await.is_err() {
                leave(&state, tag, me);
                return;
            }
        }
        Err(refusal) => {
            let _ = out.send(&ServerMsg::Refused(refusal)).await;
            // Closing at once can discard the refusal in flight; let the member hang up first.
            let _ = tokio::time::timeout(REFUSAL_LINGER, link.quic().closed()).await;
            return;
        }
    }

    let writer = {
        let link = link.clone();
        tokio::spawn(async move {
            while let Some((frame_id, frame)) = video_rx.recv().await {
                let sent = link.send_frame(frame_id, &frame).await;
                release(&queued, frame.len());
                if sent.is_err() {
                    break;
                }
            }
        })
    };
    let video = {
        let (link, state) = (link.clone(), state.clone());
        tokio::spawn(async move {
            while let Ok((frame_id, sealed)) = link.recv_frame().await {
                fan_out_video(&state, &tag, me, frame_id, sealed, limits.queue_bytes);
            }
        })
    };
    let audio = {
        let (conn, state) = (link.quic().clone(), state.clone());
        tokio::spawn(async move {
            while let Ok(packet) = conn.read_datagram().await {
                fan_out_audio(&state, &tag, me, &packet);
            }
        })
    };

    // Later control messages carry nothing yet; reading them detects the link closing.
    while input.recv::<ClientMsg>().await.is_ok() {}

    leave(&state, tag, me);
    writer.abort();
    video.abort();
    audio.abort();
}

fn fan_out_video(state: &Shared, tag: &[u8; 32], me: PublicId, frame_id: u32, sealed: Vec<u8>, budget: usize) {
    let len = sealed.len();
    let frame = Arc::new(sealed);
    for (queue, queued) in others(state, tag, me, |m| (m.video.clone(), m.queued.clone())) {
        if reserve(&queued, len, budget) && queue.try_send((frame_id, frame.clone())).is_err() {
            release(&queued, len);
        }
    }
}

/// Reserves `len` bytes of a receiver's budget. Atomic, so concurrent senders in one room
/// cannot overshoot the budget together.
fn reserve(queued: &AtomicUsize, len: usize, budget: usize) -> bool {
    queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |now| {
            now.checked_add(len).filter(|total| *total <= budget)
        })
        .is_ok()
}

fn release(queued: &AtomicUsize, len: usize) {
    let _ = queued.fetch_update(Ordering::AcqRel, Ordering::Acquire, |now| Some(now.saturating_sub(len)));
}

fn fan_out_audio(state: &Shared, tag: &[u8; 32], me: PublicId, packet: &Bytes) {
    for conn in others(state, tag, me, |m| m.conn.clone()) {
        // Audio is loss tolerant: a full or too-small path drops the packet.
        let _ = conn.send_datagram(packet.clone());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn byte_budget_refuses_frames_past_the_limit_and_frees_on_send() {
        let queued = AtomicUsize::new(0);
        assert!(reserve(&queued, 40, 100));
        assert!(reserve(&queued, 60, 100));
        assert!(!reserve(&queued, 1, 100), "budget is full");
        assert_eq!(queued.load(Ordering::Acquire), 100);
        release(&queued, 60);
        assert!(reserve(&queued, 50, 100));
        assert!(!reserve(&queued, usize::MAX, 100), "no overflow");
        assert_eq!(queued.load(Ordering::Acquire), 90);
    }
}
