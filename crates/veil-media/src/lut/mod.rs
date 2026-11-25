//! Imported colour looks: `.cube` 3D LUT files as camera effects.
//!
//! Purpose: [`load`] reads an untrusted `.cube` file ([`cube`]) and pre-computes it into a
//! bounded Y,U,V grid ([`table`]) that the camera thread applies to each NV12 picture.
//! See docs/adr/0008-lut-effects-and-effect-strength.md.
//!
//! Invariants:
//! - A file that does not parse gives a [`LutError`] whose text can be shown as it is. Nothing
//!   about the file is logged.
//! - The grid size is fixed, so memory and per-picture cost do not depend on the file.

pub mod cube;
pub mod table;
#[cfg(test)]
mod tests;

pub use cube::{Cube, LutError, MAX_FILE};
pub use table::YuvLut;

/// Parses a `.cube` file and builds its picture table.
pub fn load(bytes: &[u8]) -> Result<YuvLut, LutError> {
    Ok(YuvLut::from_cube(&cube::parse(bytes)?))
}
