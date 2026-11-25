//! One outgoing sound stream, encoded: your microphone, or the computer sound of your share.
//!
//! Purpose: each stream has its own Opus encoder and sequence numbers, so receivers can put
//! each back in order, mix, mute and measure it on its own. [`SoundOut::payload`] turns one
//! 20 ms frame into the header and Opus packet that the session then seals.
//!
//! Invariants:
//! - The encoder is made on the first frame, so a stream that never sends costs nothing.
//! - Each stream follows the data use choice's bitrate for it (`DataUse::opus_bitrate`,
//!   `DataUse::share_sound_bitrate`), changed at most once per change.
//! - A frame of the wrong length is dropped, never padded.

use veil_media::{AudioHeader, AudioSettings, OpusEncoder};

pub struct SoundOut {
    settings: AudioSettings,
    opus: Option<OpusEncoder>,
    /// The bitrate `opus` runs at.
    bitrate: u32,
    sequence: u32,
    /// Marked as a share's computer sound rather than the microphone.
    share: bool,
}

impl SoundOut {
    pub fn voice() -> Self {
        Self::new(AudioSettings::voice(), false)
    }

    pub fn share() -> Self {
        Self::new(AudioSettings::share_sound(), true)
    }

    fn new(settings: AudioSettings, share: bool) -> Self {
        Self {
            bitrate: settings.bitrate,
            settings,
            opus: None,
            sequence: 0,
            share,
        }
    }

    /// The header and Opus packet for `pcm`, ready to seal. `bitrate` changes the rate for
    /// this frame on; `None` keeps the stream's own. `None` back means nothing to send.
    pub fn payload(
        &mut self,
        pcm: &[f32],
        bitrate: Option<u32>,
        timestamp_us: u64,
    ) -> Option<Vec<u8>> {
        let wanted = bitrate.unwrap_or(self.bitrate);
        if self.opus.is_none() {
            let settings = AudioSettings {
                bitrate: wanted,
                ..self.settings
            };
            match OpusEncoder::new(&settings) {
                Ok(encoder) => {
                    self.opus = Some(encoder);
                    self.bitrate = wanted;
                }
                Err(e) => {
                    crate::note!("no audio encoder: {e}");
                    return None;
                }
            }
        }
        let opus = self.opus.as_mut()?;
        if self.bitrate != wanted {
            match opus.set_bitrate(wanted) {
                Ok(()) => self.bitrate = wanted,
                // Tried again on the next frame; the old rate still works meanwhile.
                Err(e) => crate::note!("cannot change the sound bitrate: {e}"),
            }
        }
        if pcm.len() != opus.frame_len() {
            return None;
        }
        let packet = opus.encode(pcm).ok()?;
        self.sequence = self.sequence.wrapping_add(1);
        let header = AudioHeader {
            sequence: self.sequence,
            timestamp_us,
            share: self.share,
        };
        header.write(&packet).ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn each_stream_is_marked_and_numbered_on_its_own() {
        let (mut voice, mut share) = (SoundOut::voice(), SoundOut::share());
        for expected in 1..=3 {
            let mic = voice.payload(&[0.0; 960], Some(24_000), 0).unwrap();
            let (header, _) = AudioHeader::read(&mic).unwrap();
            assert_eq!((header.sequence, header.share), (expected, false));
        }
        let sound = share.payload(&[0.0; 1_920], None, 5).unwrap();
        let (header, _) = AudioHeader::read(&sound).unwrap();
        assert_eq!(
            (header.sequence, header.share, header.timestamp_us),
            (1, true, 5)
        );
        assert_eq!(voice.bitrate, 24_000);
        assert_eq!(share.bitrate, 64_000);
    }

    #[test]
    fn a_frame_of_the_wrong_length_is_not_sent() {
        let mut share = SoundOut::share();
        assert!(
            share.payload(&[0.0; 960], None, 0).is_none(),
            "mono into stereo"
        );
        assert_eq!(share.sequence, 0);
    }
}
