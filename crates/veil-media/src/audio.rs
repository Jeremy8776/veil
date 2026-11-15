//! Audio settings and the header in front of every Opus packet, inside the sealed datagram.
//!
//! Purpose: what [`crate::OpusEncoder`] and [`crate::OpusDecoder`] are configured with, and how
//! a receiver puts packets back in order. Audio travels as unreliable datagrams, so packets
//! arrive late, out of order or not at all; the sequence number shows which.
//!
//! Wire format (version 1, big-endian), then one Opus packet:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0 | version, 1 |
//! | 1..5 | sequence number, one per packet, wrapping |
//! | 5..13 | capture time of the packet's first sample in microseconds, sender's own clock |
//!
//! Invariants:
//! - Always 48 kHz. Opus decodes any packet at any rate, so receivers never negotiate.
//! - A header parses only if its version is known and an Opus packet of 1 to
//!   [`MAX_PACKET_LEN`] bytes follows.

use crate::MediaError;

pub const SAMPLE_RATE: u32 = 48_000;
/// Largest packet the encoder may produce; libopus's own recommended buffer size.
pub const MAX_PACKET_LEN: usize = 4000;
/// Samples per channel in the longest Opus packet (120 ms).
pub const MAX_FRAME_SAMPLES: usize = SAMPLE_RATE as usize * 120 / 1000;

pub const AUDIO_VERSION: u8 = 1;
pub const AUDIO_HEADER_LEN: usize = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    pub fn count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSettings {
    pub channels: Channels,
    /// Frame length: 10, 20, 40 or 60 ms. Longer frames cost less header overhead and more
    /// delay.
    pub frame_ms: u8,
    /// Target bits per second, 6000 to 510000.
    pub bitrate: u32,
    /// In-band forward error correction: each packet carries a coarse copy of the one before.
    pub fec: bool,
    /// The loss rate to plan FEC for, 0 to 100.
    pub expected_loss_percent: u8,
    /// Discontinuous transmission: tiny packets during silence.
    pub dtx: bool,
    /// Encoder effort, 0 to 10.
    pub complexity: u8,
}

impl AudioSettings {
    /// Speech in a call: mono, 20 ms, 32 kbit/s, FEC on for 10% loss, DTX off.
    pub fn voice() -> Self {
        Self {
            channels: Channels::Mono,
            frame_ms: 20,
            bitrate: 32_000,
            fec: true,
            expected_loss_percent: 10,
            dtx: false,
            complexity: 9,
        }
    }

    /// Samples per channel in one frame.
    pub fn frame_samples(&self) -> usize {
        SAMPLE_RATE as usize * usize::from(self.frame_ms) / 1000
    }

    pub fn check(&self) -> Result<(), MediaError> {
        if ![10, 20, 40, 60].contains(&self.frame_ms) {
            return Err(MediaError::BadConfig("audio frame length"));
        }
        Self::check_bitrate(self.bitrate)?;
        if self.expected_loss_percent > 100 {
            return Err(MediaError::BadConfig("expected loss"));
        }
        if self.complexity > 10 {
            return Err(MediaError::BadConfig("audio complexity"));
        }
        Ok(())
    }

    pub(crate) fn check_bitrate(bitrate: u32) -> Result<i32, MediaError> {
        if !(6_000..=510_000).contains(&bitrate) {
            return Err(MediaError::BadConfig("audio bitrate"));
        }
        i32::try_from(bitrate).map_err(|_| MediaError::BadConfig("audio bitrate"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHeader {
    pub sequence: u32,
    pub timestamp_us: u64,
}

impl AudioHeader {
    /// The header followed by `packet`, ready to seal.
    pub fn write(&self, packet: &[u8]) -> Result<Vec<u8>, MediaError> {
        check_packet(packet)?;
        let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + packet.len());
        out.push(AUDIO_VERSION);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.timestamp_us.to_be_bytes());
        out.extend_from_slice(packet);
        Ok(out)
    }

    /// Splits an opened datagram into its header and Opus packet.
    pub fn read(payload: &[u8]) -> Result<(Self, &[u8]), MediaError> {
        let (head, packet) = payload
            .split_at_checked(AUDIO_HEADER_LEN)
            .ok_or(MediaError::Malformed("short audio payload"))?;
        let (version, rest) = head
            .split_first()
            .ok_or(MediaError::Malformed("short audio payload"))?;
        if *version != AUDIO_VERSION {
            return Err(MediaError::Malformed("audio version"));
        }
        let (sequence, timestamp) = rest.split_at(4);
        let sequence = u32::from_be_bytes(
            sequence
                .try_into()
                .map_err(|_| MediaError::Malformed("audio sequence"))?,
        );
        let timestamp_us = u64::from_be_bytes(
            timestamp
                .try_into()
                .map_err(|_| MediaError::Malformed("audio time"))?,
        );
        check_packet(packet)?;
        Ok((
            Self {
                sequence,
                timestamp_us,
            },
            packet,
        ))
    }
}

fn check_packet(packet: &[u8]) -> Result<(), MediaError> {
    if packet.is_empty() {
        return Err(MediaError::Malformed("empty opus packet"));
    }
    if packet.len() > MAX_PACKET_LEN {
        return Err(MediaError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() -> Result<(), MediaError> {
        let header = AudioHeader {
            sequence: u32::MAX,
            timestamp_us: 123_456_789,
        };
        let payload = header.write(&[0xF8, 1, 2])?;
        assert_eq!(payload.len(), AUDIO_HEADER_LEN + 3);
        let (back, packet) = AudioHeader::read(&payload)?;
        assert_eq!(back, header);
        assert_eq!(packet, &[0xF8, 1, 2]);
        Ok(())
    }

    #[test]
    fn header_refuses_bad_input() -> Result<(), MediaError> {
        let header = AudioHeader {
            sequence: 1,
            timestamp_us: 2,
        };
        assert_eq!(
            header.write(&[]),
            Err(MediaError::Malformed("empty opus packet"))
        );
        assert_eq!(
            header.write(&[0; MAX_PACKET_LEN + 1]),
            Err(MediaError::TooLarge)
        );

        let mut payload = header.write(&[1])?;
        assert!(AudioHeader::read(&payload[..AUDIO_HEADER_LEN]).is_err());
        assert!(AudioHeader::read(&payload[..4]).is_err());
        payload[0] = 2;
        assert_eq!(
            AudioHeader::read(&payload),
            Err(MediaError::Malformed("audio version"))
        );
        Ok(())
    }

    #[test]
    fn settings_are_checked() {
        assert_eq!(AudioSettings::voice().check(), Ok(()));
        assert_eq!(AudioSettings::voice().frame_samples(), 960);
        let bad = [
            AudioSettings {
                frame_ms: 15,
                ..AudioSettings::voice()
            },
            AudioSettings {
                bitrate: 5_999,
                ..AudioSettings::voice()
            },
            AudioSettings {
                bitrate: 510_001,
                ..AudioSettings::voice()
            },
            AudioSettings {
                expected_loss_percent: 101,
                ..AudioSettings::voice()
            },
            AudioSettings {
                complexity: 11,
                ..AudioSettings::voice()
            },
        ];
        for settings in bad {
            assert!(settings.check().is_err(), "{settings:?}");
        }
    }
}
