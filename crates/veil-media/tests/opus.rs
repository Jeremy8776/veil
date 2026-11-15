//! libopus end to end: a tone through the encoder and decoder, loss concealment and FEC
//! recovery, and packets that must be refused.

use std::f32::consts::TAU;

use veil_media::audio::MAX_FRAME_SAMPLES;
use veil_media::{AudioSettings, MediaError, OpusDecoder, OpusEncoder, opus_version};

/// `frames` frames of a 440 Hz tone at half scale, mono 48 kHz.
fn tone(settings: &AudioSettings, frames: usize) -> Vec<Vec<f32>> {
    let n = settings.frame_samples();
    (0..frames)
        .map(|f| {
            (0..n)
                .map(|i| 0.5 * (TAU * 440.0 * (f * n + i) as f32 / 48_000.0).sin())
                .collect()
        })
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[test]
fn links_the_vendored_libopus() {
    assert!(
        opus_version().starts_with("libopus 1.6.1"),
        "{}",
        opus_version()
    );
}

#[test]
fn a_tone_survives_encode_and_decode() -> Result<(), MediaError> {
    let settings = AudioSettings::voice();
    let mut encoder = OpusEncoder::new(&settings)?;
    let mut decoder = OpusDecoder::new(&settings)?;
    assert_eq!(encoder.frame_len(), 960);

    let mut out = Vec::new();
    let mut bytes = 0;
    for frame in tone(&settings, 50) {
        let packet = encoder.encode(&frame)?;
        bytes += packet.len();
        out.extend_from_slice(decoder.decode(&packet)?);
    }
    assert_eq!(out.len(), 50 * 960);
    // One second at 32 kbit/s is about 4000 bytes; VBR lets it drift.
    assert!((2_000..8_000).contains(&bytes), "{bytes} bytes");

    // Skip the first 100 ms (encoder look-ahead and start-up).
    let tail = &out[4_800..];
    let level = rms(tail);
    assert!((0.25..0.5).contains(&level), "rms {level}");
    // A 440 Hz tone crosses zero 880 times a second.
    let crossings = tail
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    let expected = 880.0 * tail.len() as f32 / 48_000.0;
    assert!(
        (crossings as f32 - expected).abs() < expected * 0.05,
        "{crossings} crossings"
    );
    Ok(())
}

#[test]
fn lost_frames_are_concealed_or_recovered() -> Result<(), MediaError> {
    let settings = AudioSettings {
        expected_loss_percent: 30,
        ..AudioSettings::voice()
    };
    let mut encoder = OpusEncoder::new(&settings)?;
    let mut decoder = OpusDecoder::new(&settings)?;
    let packets = tone(&settings, 10)
        .iter()
        .map(|frame| encoder.encode(frame))
        .collect::<Result<Vec<_>, _>>()?;

    for packet in &packets[..4] {
        decoder.decode(packet)?;
    }
    // Packet 4 is lost and 5 has arrived: recover 4 from 5's redundancy.
    assert_eq!(decoder.recover(&packets[5], 960)?.len(), 960);
    assert_eq!(decoder.decode(&packets[5])?.len(), 960);
    // Packet 6 is lost with nothing after it yet: conceal.
    assert_eq!(decoder.conceal(960)?.len(), 960);
    assert_eq!(decoder.decode(&packets[7])?.len(), 960);
    Ok(())
}

#[test]
fn bad_input_is_refused() -> Result<(), MediaError> {
    let settings = AudioSettings::voice();
    let mut encoder = OpusEncoder::new(&settings)?;
    let mut decoder = OpusDecoder::new(&settings)?;

    assert!(encoder.encode(&[0.0; 959]).is_err());
    assert!(encoder.set_bitrate(1_000).is_err());
    assert!(encoder.set_expected_loss(101).is_err());
    assert!(
        OpusEncoder::new(&AudioSettings {
            frame_ms: 5,
            ..settings
        })
        .is_err()
    );

    assert!(decoder.decode(&[]).is_err());
    // Code 3 (several frames) with no frame-count byte.
    assert!(decoder.decode(&[0xFF]).is_err());
    assert!(decoder.conceal(0).is_err());
    assert!(decoder.conceal(100).is_err());
    assert!(decoder.conceal(MAX_FRAME_SAMPLES + 120).is_err());
    assert!(decoder.recover(&[], 960).is_err());

    // The decoder still works after refusing.
    let packet = encoder.encode(&[0.0; 960])?;
    assert_eq!(decoder.decode(&packet)?.len(), 960);
    Ok(())
}
