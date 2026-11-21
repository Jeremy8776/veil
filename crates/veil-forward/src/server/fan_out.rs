//! Passing members' media on: video frames within each receiver's budget, audio as
//! datagrams, and sealed app messages. Nothing here reads a frame's content; the forwarder
//! only sees each frame's layer, to honour subscriptions.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use veil_net::control::MAX_APP_LEN;
use veil_security::PublicId;
use veil_security::frame::frame_layer;

use super::budget::{Budgets, reserve_all};
use super::{Frame, Limits, Member, RoomKey, Shared, lock};
use crate::protocol::ServerMsg;

/// Passes a sealed app message from member `me` (this link `id`) to everyone else in the
/// room. Too large, or from a replaced link, it is dropped; a full control queue drops it
/// for that receiver only.
pub(super) fn fan_out_app(state: &Shared, key: &RoomKey, me: PublicId, id: u64, sealed: Vec<u8>) {
    if sealed.len() > MAX_APP_LEN {
        return;
    }
    let Some(s) = lock(state) else { return };
    let Some(room) = s.rooms.get(key) else { return };
    if !room.members.get(&me).is_some_and(|m| m.id == id) {
        return;
    }
    for (_, member) in room.members.iter().filter(|(device, _)| **device != me) {
        let _ = member.control.try_send(ServerMsg::App(sealed.clone()));
    }
}

/// Everyone in the room except the sender.
fn others<T>(state: &Shared, key: &RoomKey, me: PublicId, pick: impl Fn(&Member) -> T) -> Vec<T> {
    lock(state)
        .and_then(|s| {
            s.rooms.get(key).map(|room| {
                room.members
                    .iter()
                    .filter(|(device, _)| **device != me)
                    .map(|(_, m)| pick(m))
                    .collect()
            })
        })
        .unwrap_or_default()
}

pub(super) fn fan_out_video(
    state: &Shared,
    budgets: &Arc<Budgets>,
    limits: &Limits,
    key: &RoomKey,
    me: PublicId,
    frame_id: u32,
    sealed: Vec<u8>,
) {
    let Ok(layer) = frame_layer(&sealed) else {
        return;
    };
    let receivers: Vec<_> = lock(state)
        .and_then(|mut s| {
            s.rooms.get_mut(key).map(|room| {
                room.members
                    .iter_mut()
                    .filter(|(device, _)| **device != me)
                    .filter_map(|(_, m)| {
                        m.subs
                            .pass(&me, layer)
                            .then(|| (m.video.clone(), m.queued.clone(), m.source))
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    let len = sealed.len();
    let frame = Arc::new(sealed);
    for (queue, queued, source) in receivers {
        if let Some(reservation) = reserve_all(budgets, &queued, source, len, limits) {
            // A full queue hands the frame back; dropping it releases the reservation.
            let _ = queue.try_send(Frame {
                id: frame_id,
                sealed: frame.clone(),
                enqueued: Instant::now(),
                _reservation: reservation,
            });
        }
    }
}

pub(super) fn fan_out_audio(state: &Shared, key: &RoomKey, me: PublicId, packet: &Bytes) {
    for conn in others(state, key, me, |m| m.conn.clone()) {
        // Audio is loss tolerant: a full or too-small path drops the packet.
        let _ = conn.send_datagram(packet.clone());
    }
}
