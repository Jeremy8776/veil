//! libopus end to end: a tone through the encoder and decoder, loss concealment and FEC
//! recovery, and packets that must be refused.

use std::f32::consts::TAU;

use veil_media::audio::MAX_FRAME_SAMPLES;
use veil_media::{
    AudioHeader, AudioSettings, JitterBuffer, MediaError, OpusDecoder, OpusEncoder, Playout,
    opus_version,
};

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

/// A tone sent over a simulated network (10 to 50 ms delay, every 7th packet lost) through the
/// jitter buffer: every tick of the playback clock yields one frame, losses are rebuilt from
/// FEC, and the tone keeps most of its level.
#[test]
fn the_jitter_buffer_feeds_the_decoder_through_loss() -> Result<(), MediaError> {
    let settings = AudioSettings::voice();
    let n = settings.frame_samples();
    let mut encoder = OpusEncoder::new(&settings)?;
    let mut decoder = OpusDecoder::new(&settings)?;
    let mut buffer = JitterBuffer::new(settings.frame_ms);
    let mut arrivals: Vec<(u64, AudioHeader, Vec<u8>)> = Vec::new();
    for (seq, frame) in tone(&settings, 60).iter().enumerate() {
        let header = AudioHeader {
            sequence: seq as u32,
            timestamp_us: seq as u64 * 20_000,
            share: false,
        };
        let delay = 10_000 + (seq as u64 * 37 % 5) * 10_000;
        let packet = encoder.encode(frame)?;
        if seq % 7 != 3 {
            arrivals.push((header.timestamp_us + delay, header, packet));
        }
    }
    arrivals.sort_by_key(|a| a.0);
    let mut arrivals = arrivals.into_iter().peekable();
    let (mut recovered, mut concealed, mut played) = (0, 0, Vec::new());
    for tick in 0..70u64 {
        let now = tick * 20_000;
        while let Some((at, header, packet)) = arrivals.next_if(|a| a.0 <= now) {
            buffer.push(header, &packet, at);
        }
        let pcm = match buffer.pop() {
            Playout::Packet(p) => decoder.decode(&p)?.to_vec(),
            Playout::Recover(next) => {
                recovered += 1;
                decoder.recover(&next, n)?.to_vec()
            }
            Playout::Conceal => {
                // After tick 60 the sender has stopped; only count gaps while it sends.
                concealed += u32::from(tick < 60);
                decoder.conceal(n)?.to_vec()
            }
            Playout::Wait => vec![0.0; n],
        };
        assert_eq!(pcm.len(), n);
        played.extend(pcm);
    }
    eprintln!("recovered {recovered}, concealed {concealed}");
    assert!(recovered >= 6, "only {recovered} frames rebuilt from FEC");
    // Only while the delay grows to cover the jitter (target 3 frames): every loss is rebuilt.
    assert!(concealed <= 4, "{concealed} frames concealed");
    // Skip the start, while the buffer fills and the decoder settles.
    let level = rms(&played[n * 10..n * 60]);
    assert!(level > 0.25, "tone level {level:.3}");
    Ok(())
}

/// Constant bitrate: speech, silence and a loud tone all give packets of one size, so the
/// sizes say nothing about what was said.
#[test]
fn voice_packets_are_all_one_size() -> Result<(), MediaError> {
    let settings = AudioSettings::voice();
    assert!(settings.constant_bitrate);
    let mut encoder = OpusEncoder::new(&settings)?;
    let mut frames = tone(&settings, 10);
    frames.extend(std::iter::repeat_n(vec![0.0; 960], 10));
    frames.extend(tone(&settings, 10).into_iter().map(|f| f.iter().map(|s| s * 0.05).collect()));
    let sizes: std::collections::BTreeSet<usize> = frames
        .iter()
        .map(|frame| encoder.encode(frame).map(|p| p.len()))
        .collect::<Result<_, _>>()?;
    // 32 kbit/s for 20 ms is 80 bytes.
    assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![80], "sizes vary");

    let mut vbr = OpusEncoder::new(&AudioSettings {
        constant_bitrate: false,
        ..settings
    })?;
    let vbr_sizes: std::collections::BTreeSet<usize> = frames
        .iter()
        .map(|frame| vbr.encode(frame).map(|p| p.len()))
        .collect::<Result<_, _>>()?;
    assert!(vbr_sizes.len() > 1, "the test can tell the two modes apart");
    Ok(())
}
