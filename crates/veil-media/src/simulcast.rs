//! Simulcast: one independent encoder per ladder rung, behind one [`VideoEncoder`].
//!
//! Purpose: hardware H.264 has no spatial SVC, so each rung is its own stream at its own size
//! (`needs_lower` is always false). This module decides which rung encoders see each picture
//! and checks what they return. The platform part is a [`RungEncoder`]: one picture in, one
//! Annex B access unit out.
//!
//! How temporal layers stay true. A rung's encoder runs its own dyadic temporal pattern with
//! `top_temporal + 1` levels, counted from when it started (an IDR does not restart it, and
//! encoders honour a forced IDR only at the pattern's start). Fed every picture with
//! `temporal <= top_temporal`, that pattern lines up with the [`crate::Schedule`]'s, and each
//! picture references exactly what the plan says. So a rung is fed those pictures even when
//! the plan lowered its temporal layers, and the output is thrown away. Dropping only the
//! highest layers loses no picture any kept picture refers to.
//!
//! Invariants:
//! - A rung is live from a keyframe that lists it until a temporal 0 picture that does not
//!   (temporal 0 is always encoded for every wanted rung, so its absence means paused).
//!   A paused rung is not fed, and its encoder is restarted ([`RungEncoder::restart`]) at the
//!   next keyframe, so its pattern starts again in step with the schedule.
//! - A planned keyframe must come out as an IDR, and a prefix unit's temporal id, when the
//!   encoder writes one, must match the plan. Otherwise the picture is refused.
//! - Call [`VideoEncoder::encode`] for every captured picture, including plans with no
//!   layers, so each rung's pattern keeps counting.

use crate::MediaError;
use crate::annexb;
use crate::encoder::{EncodedLayer, RawPicture, VideoEncoder, check_output};
use crate::ladder::{Ladder, Rung};
use crate::payload::Codec;
use crate::scale::scale_nv12;
use crate::schedule::PicturePlan;

/// One rung's platform encoder. Its size, frame rate, bitrate and temporal level count
/// (`top_temporal + 1`) are fixed when it is made.
pub trait RungEncoder {
    /// Encodes one tightly packed NV12 picture at the rung's size into one access unit.
    fn encode(
        &mut self,
        nv12: &[u8],
        timestamp_us: u64,
        keyframe: bool,
    ) -> Result<Vec<u8>, MediaError>;

    /// Starts a fresh stream: the next picture is the first of the temporal pattern.
    fn restart(&mut self) -> Result<(), MediaError>;
}

struct Slot<E> {
    rung: Rung,
    encoder: E,
    live: bool,
    /// Has encoded at least one picture, so a restart needs a fresh stream.
    started: bool,
    scaled: Vec<u8>,
}

pub struct Simulcast<E> {
    slots: Vec<Slot<E>>,
}

impl<E: RungEncoder> Simulcast<E> {
    /// Makes one encoder per rung with `make`, lowest rung first.
    pub fn new(
        ladder: &Ladder,
        mut make: impl FnMut(&Rung, u32) -> Result<E, MediaError>,
    ) -> Result<Self, MediaError> {
        let slots = ladder
            .rungs()
            .iter()
            .map(|rung| {
                Ok(Slot {
                    rung: *rung,
                    encoder: make(rung, ladder.fps(rung))?,
                    live: false,
                    started: false,
                    scaled: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, MediaError>>()?;
        Ok(Self { slots })
    }

    /// Each rung's encoder, lowest first.
    pub fn encoders(&self) -> impl Iterator<Item = &E> {
        self.slots.iter().map(|slot| &slot.encoder)
    }
}

impl<E: RungEncoder> VideoEncoder for Simulcast<E> {
    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn encode(
        &mut self,
        picture: &RawPicture<'_>,
        plan: &PicturePlan,
    ) -> Result<Vec<EncodedLayer>, MediaError> {
        let mut out = Vec::with_capacity(plan.layers.len());
        for slot in &mut self.slots {
            let spatial = slot.rung.tier.spatial();
            let listed = plan.layers.iter().find(|layer| layer.spatial == spatial);
            if plan.temporal > slot.rung.top_temporal {
                continue;
            }
            if listed.is_none() && plan.temporal == 0 {
                slot.live = false;
            }
            if listed.is_none() && !slot.live {
                continue;
            }
            if !slot.live && !plan.keyframe && plan.temporal != 0 {
                return Err(MediaError::Encoder("layer planned without its reference"));
            }
            if slot.rung.width > picture.width || slot.rung.height > picture.height {
                return Err(MediaError::BadConfig("picture smaller than the ladder"));
            }
            // A rung that was paused restarts on an IDR even when the plan does not ask.
            let keyframe = plan.keyframe || !slot.live;
            if !slot.live && slot.started {
                slot.encoder.restart()?;
            }
            scale_nv12(picture, slot.rung.width, slot.rung.height, &mut slot.scaled);
            let bitstream = slot
                .encoder
                .encode(&slot.scaled, picture.timestamp_us, keyframe)?;
            if keyframe && !annexb::has_idr(&bitstream) {
                return Err(MediaError::Encoder("keyframe not honoured"));
            }
            if annexb::temporal_id(&bitstream).is_some_and(|id| id != plan.temporal) {
                return Err(MediaError::Encoder("temporal layer differs from the plan"));
            }
            slot.live = true;
            slot.started = true;
            if let Some(layer) = listed {
                out.push(EncodedLayer {
                    layer: *layer,
                    width: slot.rung.width,
                    height: slot.rung.height,
                    needs_lower: false,
                    bitstream,
                });
            }
        }
        check_output(plan, &out)?;
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Instant;

    use veil_security::layer::Wanted;

    use super::*;
    use crate::ladder::Tier;
    use crate::schedule::Schedule;

    /// Behaves like the Media Foundation encoders: a prefix unit with the temporal id of its
    /// own pattern, counted from start or restart, and a forced IDR only at temporal 0.
    struct Fake {
        levels: u8,
        count: u64,
        calls: Vec<bool>,
        restarts: usize,
    }

    impl RungEncoder for Fake {
        fn encode(&mut self, nv12: &[u8], _: u64, keyframe: bool) -> Result<Vec<u8>, MediaError> {
            let temporal = crate::temporal::temporal_id(self.count, self.levels);
            self.count += 1;
            self.calls.push(keyframe);
            let nal = if keyframe && temporal == 0 {
                0x65
            } else {
                0x41
            };
            Ok(vec![
                0,
                0,
                1,
                0x6e,
                0x80,
                0,
                temporal << 5,
                0,
                0,
                1,
                nal,
                nv12[0],
            ])
        }

        fn restart(&mut self) -> Result<(), MediaError> {
            self.count = 0;
            self.restarts += 1;
            Ok(())
        }
    }

    /// The camera ladder's shape at a sixteenth of the size, so tests scale quickly.
    fn small() -> Ladder {
        let rung = |tier, width, height, top_temporal| Rung {
            tier,
            width,
            height,
            top_temporal,
            bitrate_bps: 1,
        };
        let rungs = vec![
            rung(Tier::Thumb, 40, 22, 0),
            rung(Tier::Hd, 120, 68, 1),
            rung(Tier::Uhd, 240, 136, 2),
        ];
        Ladder::new(60, 3, rungs).unwrap()
    }

    fn simulcast() -> Simulcast<Fake> {
        Simulcast::new(&small(), |rung, _| {
            Ok(Fake {
                levels: rung.top_temporal + 1,
                count: 0,
                calls: Vec::new(),
                restarts: 0,
            })
        })
        .unwrap()
    }

    fn uhd_picture(data: &[u8]) -> RawPicture<'_> {
        RawPicture::new(240, 136, 240, 0, data).unwrap()
    }

    #[test]
    fn rungs_follow_the_schedule_and_its_temporal_layers() {
        let data = vec![9u8; 240 * 136 * 3 / 2];
        let now = Instant::now();
        let mut schedule = Schedule::new(small());
        let mut encoder = simulcast();
        for _ in 0..12 {
            let plan = schedule.next(now);
            let out = encoder.encode(&uhd_picture(&data), &plan).unwrap();
            assert!(
                out.iter()
                    .all(|layer| !layer.needs_lower && layer.bitstream.ends_with(&[9]))
            );
        }
        let calls: Vec<usize> = encoder.encoders().map(|e| e.calls.len()).collect();
        assert_eq!(calls, [3, 6, 12], "Thumb 15 fps, Hd 30 fps, Uhd 60 fps");
    }

    #[test]
    fn lowered_temporal_layers_are_still_encoded_and_dropped() {
        let data = vec![1u8; 240 * 136 * 3 / 2];
        let now = Instant::now();
        let mut schedule = Schedule::new(small());
        let mut encoder = simulcast();
        encoder
            .encode(&uhd_picture(&data), &schedule.next(now))
            .unwrap();
        schedule.set_wanted(Some(Wanted {
            spatial: 2,
            temporal: 0,
        }));
        for _ in 0..7 {
            let plan = schedule.next(now);
            let out = encoder.encode(&uhd_picture(&data), &plan).unwrap();
            assert!(out.iter().all(|layer| layer.layer.temporal == 0));
        }
        let uhd = encoder.encoders().nth(2).unwrap();
        assert_eq!(
            uhd.calls.len(),
            8,
            "every picture still reaches the Uhd encoder"
        );
    }

    #[test]
    fn a_paused_rung_stops_and_restarts_on_a_keyframe() {
        let data = vec![1u8; 240 * 136 * 3 / 2];
        let now = Instant::now();
        let mut schedule = Schedule::new(small());
        let mut encoder = simulcast();
        encoder
            .encode(&uhd_picture(&data), &schedule.next(now))
            .unwrap();
        schedule.set_wanted(Some(Wanted {
            spatial: Tier::Hd.spatial(),
            temporal: 3,
        }));
        for _ in 0..7 {
            encoder
                .encode(&uhd_picture(&data), &schedule.next(now))
                .unwrap();
        }
        let before = encoder.encoders().nth(2).unwrap().calls.len();
        assert!(
            before < 8,
            "Uhd stops at the first temporal 0 picture without it"
        );
        schedule.set_wanted(Some(Wanted::ALL));
        let later = now + std::time::Duration::from_secs(1);
        // The keyframe waits for the pattern boundary; every picture still goes through.
        let mut out = Vec::new();
        while out.len() < 3 {
            let plan = schedule.next(later);
            out = encoder.encode(&uhd_picture(&data), &plan).unwrap();
        }
        assert!(out.iter().all(|layer| layer.layer.keyframe));
        let uhd = encoder.encoders().nth(2).unwrap();
        assert_eq!((uhd.calls.last(), uhd.restarts), (Some(&true), 1));
    }

    #[test]
    fn a_wrong_temporal_id_or_missing_idr_is_refused() {
        struct Liar;
        impl RungEncoder for Liar {
            fn encode(&mut self, _: &[u8], _: u64, _: bool) -> Result<Vec<u8>, MediaError> {
                Ok(vec![0, 0, 1, 0x41, 0])
            }

            fn restart(&mut self) -> Result<(), MediaError> {
                Ok(())
            }
        }
        let ladder = Ladder::camera().up_to(Tier::Thumb);
        let mut encoder = Simulcast::new(&ladder, |_, _| Ok(Liar)).unwrap();
        let mut schedule = Schedule::new(ladder);
        let data = vec![0u8; 640 * 360 * 3 / 2];
        let picture = RawPicture::new(640, 360, 640, 0, &data).unwrap();
        let plan = schedule.next(Instant::now());
        assert_eq!(
            encoder.encode(&picture, &plan),
            Err(MediaError::Encoder("keyframe not honoured"))
        );
    }
}
