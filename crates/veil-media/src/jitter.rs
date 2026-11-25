//! The receiver's audio jitter buffer: puts Opus packets back in order and says, every frame,
//! what to play.
//!
//! Purpose: audio arrives as unreliable datagrams, late, out of order or not at all. The
//! playback clock calls [`JitterBuffer::pop`] once per frame and gets a [`Playout`]: a packet
//! to decode, the next packet to rebuild a lost one from (Opus in-band FEC), a gap to conceal,
//! or silence while the buffer fills.
//!
//! Invariants:
//! - Sequence numbers wrap at `u32`; they are extended to `u64` relative to the newest one, so
//!   a wrap is not a jump.
//! - The delay adapts to the network: the target is the worst recent arrival jitter (decaying
//!   slowly) rounded up to whole frames, plus one, between 1 and [`MAX_TARGET_FRAMES`].
//! - A missing packet counts as lost only once packets up to the target depth after it have
//!   arrived. Until then its slot is concealed without moving on, which adds one frame of
//!   delay, so a packet that is merely late still plays.
//! - Memory is bounded: at most [`MAX_PACKETS`] packets are held. A packet too far ahead resets
//!   the buffer (the sender restarted); one whose slot has already played is dropped.
//! - The restart rule works both ways: after [`RESYNC_AFTER_BEHIND`] packets in a row that are
//!   far behind the play position, the buffer starts again from them. Any call member can seal
//!   sound under another member's sender index, so one forged packet far ahead must not leave
//!   that member's real stream permanently "already played" (GHSA-9wq4-7r8v-7582).
//! - After [`REBUFFER_AFTER_MS`] of nothing to play, the buffer stops and fills again from
//!   scratch (the newest position is forgotten too), so a long gap does not leave the call
//!   permanently behind.
//! - Time is passed in (arrival in microseconds on the receiver's clock), so it is tested
//!   without sleeping.

use std::collections::BTreeMap;

use crate::audio::AudioHeader;

/// The most frames the buffer will hold back to absorb jitter.
pub const MAX_TARGET_FRAMES: usize = 10;
/// The most packets held at once.
pub const MAX_PACKETS: usize = 64;
/// Stop and fill again after this long with nothing to play.
pub const REBUFFER_AFTER_MS: u64 = 200;
/// Start again after this many packets in a row arrive far behind the play position.
pub const RESYNC_AFTER_BEHIND: u32 = 25;

/// What the playback clock does for one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Playout {
    /// Decode this packet.
    Packet(Vec<u8>),
    /// This frame was lost; rebuild it from the next packet's FEC (`OpusDecoder::recover`).
    Recover(Vec<u8>),
    /// This frame was lost with nothing to rebuild it from (`OpusDecoder::conceal`).
    Conceal,
    /// The buffer is filling: play silence.
    Wait,
}

pub struct JitterBuffer {
    frame_us: u64,
    packets: BTreeMap<u64, Vec<u8>>,
    /// The newest extended sequence number seen.
    newest: Option<u64>,
    /// The next sequence number to play; `None` while filling.
    next: Option<u64>,
    /// Worst recent arrival jitter in microseconds, decaying.
    peak_jitter_us: f64,
    /// Arrival time minus capture time of the last packet, for the jitter estimate.
    last_transit: Option<i64>,
    /// Frames played in a row with nothing to play.
    missing: u64,
    /// Packets in a row that arrived more than [`MAX_PACKETS`] behind the play position.
    behind: u32,
}

impl JitterBuffer {
    /// A buffer for packets of `frame_ms` milliseconds each (`AudioSettings::frame_ms`).
    pub fn new(frame_ms: u8) -> Self {
        Self {
            frame_us: u64::from(frame_ms.max(1)) * 1_000,
            packets: BTreeMap::new(),
            newest: None,
            next: None,
            peak_jitter_us: 0.0,
            last_transit: None,
            missing: 0,
            behind: 0,
        }
    }

    /// Takes a packet that arrived at `arrival_us` on the receiver's clock.
    pub fn push(&mut self, header: AudioHeader, packet: &[u8], arrival_us: u64) {
        let seq = self.extend(header.sequence);
        // The play position, or the oldest packet held while filling.
        let first = self.next.or(self.packets.keys().next().copied());
        let too_far = first.is_some_and(|first| seq >= first + MAX_PACKETS as u64);
        let far_behind = first.is_some_and(|first| seq.saturating_add(MAX_PACKETS as u64) <= first);
        self.behind = if far_behind { self.behind + 1 } else { 0 };
        if too_far || self.behind >= RESYNC_AFTER_BEHIND {
            // The sender restarted or skipped far ahead, or the position itself is far ahead
            // of a steady stream (one forged packet put it there): start again from here.
            self.reset();
            return self.push(header, packet, arrival_us);
        }
        if far_behind {
            return; // A straggler, or a stream the buffer has not resynced to yet.
        }
        if self.next.is_some_and(|next| seq < next) {
            return; // Its slot has played already.
        }
        self.track_jitter(header.timestamp_us, arrival_us);
        self.newest = Some(self.newest.map_or(seq, |n| n.max(seq)));
        self.packets.insert(seq, packet.to_vec());
        while self.packets.len() > MAX_PACKETS {
            self.packets.pop_first();
        }
    }

    /// What to play for the next frame. Call once per frame on the playback clock.
    pub fn pop(&mut self) -> Playout {
        let Some(next) = self.next.or_else(|| self.ready()) else {
            return Playout::Wait;
        };
        let next = self.catch_up(next);
        if let Some(packet) = self.packets.remove(&next) {
            self.next = Some(next + 1);
            self.missing = 0;
            return Playout::Packet(packet);
        }
        self.missing += 1;
        if self.missing * self.frame_us >= REBUFFER_AFTER_MS * 1_000 && self.packets.is_empty() {
            // Nothing is held, so forget the position as well: the next packet starts afresh
            // instead of being measured against a newest number that may have been forged.
            self.reset();
            return Playout::Conceal;
        }
        let ahead = self.newest.map_or(0, |n| (n + 1).saturating_sub(next));
        if ahead < self.target_frames() as u64 {
            // Not known to be lost yet: wait one frame for it.
            self.next = Some(next);
            return Playout::Conceal;
        }
        self.next = Some(next + 1);
        match self.packets.get(&(next + 1)) {
            Some(packet) => Playout::Recover(packet.clone()),
            None => Playout::Conceal,
        }
    }

    /// Frames the buffer aims to hold back.
    pub fn target_frames(&self) -> usize {
        let frames = (self.peak_jitter_us / self.frame_us as f64).ceil() as usize + 1;
        frames.clamp(1, MAX_TARGET_FRAMES)
    }

    /// Frames between the next one to play and the newest one received.
    pub fn depth_frames(&self) -> usize {
        let Some(newest) = self.newest else { return 0 };
        let from = self.next.or(self.packets.keys().next().copied());
        from.map_or(0, |f| (newest + 1).saturating_sub(f) as usize)
    }

    /// Starts playing once enough is buffered; the first sequence number to play.
    fn ready(&mut self) -> Option<u64> {
        let first = *self.packets.keys().next()?;
        let newest = self.newest?;
        (newest + 1 - first >= self.target_frames() as u64).then_some(first)
    }

    /// When the buffer holds far more than the target (the network got faster), skips the
    /// oldest packets so the delay comes back down.
    ///
    /// shortcut: skipping drops audio with a small click. Time-stretch (play slightly fast)
    /// instead if it is audible in calls.
    fn catch_up(&mut self, next: u64) -> u64 {
        let target = self.target_frames() as u64;
        let Some(newest) = self.newest else {
            return next;
        };
        if newest + 1 - next.min(newest + 1) <= target * 2 + 2 {
            return next;
        }
        let skip_to = newest + 1 - target;
        self.packets = self.packets.split_off(&skip_to);
        skip_to
    }

    /// RFC 3550-style transit difference, kept as a slowly decaying peak.
    fn track_jitter(&mut self, sent_us: u64, arrival_us: u64) {
        let transit = arrival_us as i64 - sent_us as i64;
        if let Some(last) = self.last_transit {
            let jitter = (transit - last).unsigned_abs() as f64;
            // Decays to half in about 70 packets (1.4 s at 20 ms).
            self.peak_jitter_us = jitter.max(self.peak_jitter_us * 0.99);
        }
        self.last_transit = Some(transit);
    }

    /// Extends a 32-bit sequence number to 64 bits around the newest one seen.
    fn extend(&self, seq: u32) -> u64 {
        let Some(newest) = self.newest else {
            // Start high so sequence numbers just below the first one do not underflow.
            return (1u64 << 32) + u64::from(seq);
        };
        let diff = i64::from(seq.wrapping_sub(newest as u32) as i32);
        newest.saturating_add_signed(diff)
    }

    fn reset(&mut self) {
        self.packets.clear();
        self.newest = None;
        self.next = None;
        self.last_transit = None;
        self.missing = 0;
        self.behind = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_US: u64 = 20_000;

    fn header(sequence: u32) -> AudioHeader {
        AudioHeader {
            sequence,
            timestamp_us: u64::from(sequence) * FRAME_US,
            share: false,
        }
    }

    /// Pushes packet `seq` (one byte, its low bits) arriving `late_us` after it was sent.
    fn push(buffer: &mut JitterBuffer, seq: u32, late_us: u64) {
        let h = header(seq);
        buffer.push(h, &[seq as u8], h.timestamp_us + late_us);
    }

    fn packet(seq: u32) -> Playout {
        Playout::Packet(vec![seq as u8])
    }

    #[test]
    fn a_steady_stream_plays_in_order() {
        let mut buffer = JitterBuffer::new(20);
        assert_eq!(buffer.pop(), Playout::Wait);
        for seq in 0..10 {
            push(&mut buffer, seq, 5_000);
            assert_eq!(buffer.pop(), packet(seq));
        }
        assert_eq!(buffer.target_frames(), 1);
    }

    #[test]
    fn reordered_packets_come_out_in_order() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        push(&mut buffer, 2, 0);
        push(&mut buffer, 1, 30_000);
        assert_eq!(buffer.pop(), packet(0));
        assert_eq!(buffer.pop(), packet(1));
        assert_eq!(buffer.pop(), packet(2));
    }

    #[test]
    fn a_lost_packet_is_recovered_from_the_next_one() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        assert_eq!(buffer.pop(), packet(0));
        push(&mut buffer, 2, 0);
        assert_eq!(buffer.pop(), Playout::Recover(vec![2]));
        assert_eq!(buffer.pop(), packet(2));
    }

    #[test]
    fn a_gap_with_nothing_after_it_is_concealed_then_refills() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        assert_eq!(buffer.pop(), packet(0));
        for _ in 0..10 {
            assert_eq!(buffer.pop(), Playout::Conceal);
        }
        // 200 ms of nothing: the buffer stops and waits to fill again.
        assert_eq!(buffer.pop(), Playout::Wait);
        push(&mut buffer, 30, 0);
        assert_eq!(buffer.pop(), packet(30));
    }

    #[test]
    fn a_late_packet_is_waited_for() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        assert_eq!(buffer.pop(), packet(0));
        // Nothing after it has arrived, so it may just be late: conceal and wait.
        assert_eq!(buffer.pop(), Playout::Conceal);
        push(&mut buffer, 1, 20_000);
        assert_eq!(buffer.pop(), packet(1));
    }

    #[test]
    fn a_packet_after_its_slot_is_dropped() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        assert_eq!(buffer.pop(), packet(0));
        push(&mut buffer, 2, 0);
        assert_eq!(buffer.pop(), Playout::Recover(vec![2]));
        push(&mut buffer, 1, 40_000);
        assert_eq!(buffer.pop(), packet(2));
    }

    #[test]
    fn jitter_raises_the_delay_before_playing() {
        let mut buffer = JitterBuffer::new(20);
        // Arrivals swing by 50 ms: the target becomes 4 frames (3 for 50 ms, plus one).
        push(&mut buffer, 0, 0);
        push(&mut buffer, 1, 50_000);
        assert_eq!(buffer.target_frames(), 4);
        assert_eq!(buffer.pop(), Playout::Wait);
        push(&mut buffer, 2, 0);
        push(&mut buffer, 3, 0);
        assert_eq!(buffer.pop(), packet(0));
    }

    #[test]
    fn the_delay_never_exceeds_the_cap() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        push(&mut buffer, 1, 2_000_000);
        assert_eq!(buffer.target_frames(), MAX_TARGET_FRAMES);
    }

    #[test]
    fn a_backlog_is_skipped_down_to_the_target() {
        let mut buffer = JitterBuffer::new(20);
        for seq in 0..20 {
            push(&mut buffer, seq, 0);
        }
        // Target is 1 frame; 20 queued is far over it, so play jumps to the newest.
        assert_eq!(buffer.pop(), packet(19));
        assert_eq!(buffer.depth_frames(), 0);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut buffer = JitterBuffer::new(20);
        for seq in [u32::MAX - 1, u32::MAX, 0, 1] {
            push(&mut buffer, seq, 0);
            assert_eq!(buffer.pop(), packet(seq));
        }
    }

    /// GHSA-9wq4-7r8v-7582: one packet sealed under the victim's index with a sequence number
    /// about 2^31 ahead must not silence the victim's real stream.
    #[test]
    fn one_forged_far_ahead_packet_must_not_silence_the_stream() {
        let mut buffer = JitterBuffer::new(20);
        for seq in 0..5 {
            push(&mut buffer, seq, 0);
            assert_eq!(buffer.pop(), packet(seq));
        }
        let forged = AudioHeader {
            sequence: 4 + 0x7FFF_FFFF,
            timestamp_us: 0,
            share: false,
        };
        buffer.push(forged, &[0xAA], 0);
        let mut played = 0;
        for seq in 5..505u32 {
            push(&mut buffer, seq, 0);
            if buffer.pop() == packet(seq) {
                played += 1;
            }
        }
        assert!(played > 400, "the real stream never plays again: {played}");
    }

    #[test]
    fn a_forged_capture_time_does_not_silence_the_stream() {
        let mut buffer = JitterBuffer::new(20);
        for seq in 0..5 {
            push(&mut buffer, seq, 0);
            assert_eq!(buffer.pop(), packet(seq));
        }
        let forged = AudioHeader {
            sequence: 4 + 0x7FFF_FFFF,
            timestamp_us: u64::MAX / 2,
            share: false,
        };
        buffer.push(forged, &[0xAA], 0);
        let mut played = 0;
        for seq in 5..505u32 {
            push(&mut buffer, seq, 0);
            if buffer.pop() == packet(seq) {
                played += 1;
            }
        }
        assert!(played > 400, "the real stream never plays again: {played}");
    }

    #[test]
    fn a_few_stale_packets_do_not_reset_a_live_stream() {
        let mut buffer = JitterBuffer::new(20);
        for seq in 1_000..1_010 {
            push(&mut buffer, seq, 0);
            assert_eq!(buffer.pop(), packet(seq));
        }
        // Stragglers far behind are dropped one by one; play goes on where it was.
        for seq in 0..(RESYNC_AFTER_BEHIND - 1) {
            push(&mut buffer, seq, 0);
        }
        push(&mut buffer, 1_010, 0);
        assert_eq!(buffer.pop(), packet(1_010));
    }

    #[test]
    fn a_far_jump_resets_and_memory_stays_bounded() {
        let mut buffer = JitterBuffer::new(20);
        push(&mut buffer, 0, 0);
        assert_eq!(buffer.pop(), packet(0));
        push(&mut buffer, 1_000_000, 0);
        assert_eq!(buffer.pop(), packet(1_000_000));
        let mut filling = JitterBuffer::new(20);
        for seq in 0..(MAX_PACKETS as u32 * 3) {
            push(&mut filling, seq, 0);
            assert!(filling.packets.len() <= MAX_PACKETS);
        }
    }
}
