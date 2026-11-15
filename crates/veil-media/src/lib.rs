//! Veil media pipeline: what to encode, how encoded pictures are framed, and when a receiver
//! can decode.
//!
//! Purpose: the parts of capture and encoding that do not depend on the operating system.
//! [`Schedule`] decides, for every captured picture, which spatial and temporal layers to
//! encode and whether it is a keyframe. [`VideoHeader`] goes in front of every encoded layer,
//! inside the sealed frame. [`Gate`] tells a receiver whether a layer can be decoded, and when
//! to ask the sender for a keyframe. Per-OS encoders implement [`VideoEncoder`].
//!
//! Invariants:
//! - No `unsafe` here. Native media APIs will live in one `ffi` module per OS, under the rules
//!   in `docs/TRANSPARENCY.md` section 5.
//! - Nothing here touches the network or keys. The app seals each payload with
//!   `FrameSender::encrypt_layer`, using the [`veil_security::Layer`] the schedule chose.
//! - Time is passed in, so keyframe timing is tested without sleeping.

pub mod encoder;
pub mod error;
pub mod gate;
pub mod ladder;
pub mod payload;
pub mod schedule;
pub mod temporal;

pub use encoder::{EncodedLayer, RawPicture, VideoEncoder};
pub use error::MediaError;
pub use gate::{Gate, Verdict};
pub use ladder::{Ladder, Rung, Tier};
pub use payload::{Codec, VideoHeader};
pub use schedule::{PicturePlan, Schedule};
