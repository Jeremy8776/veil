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

/// The greys a look's swatch shows, from shadow to highlight.
pub const RAMP: [f32; 5] = [0.1, 0.3, 0.5, 0.7, 0.9];

/// What a look does to each grey of [`RAMP`], as RGB in 0..1, darkest first. Shows its
/// shadow and highlight tints, and lifted or crushed blacks, without building a table.
pub fn tone_ramp(cube: &Cube) -> [[f32; 3]; RAMP.len()] {
    RAMP.map(|grey| {
        let input: [f32; 3] = std::array::from_fn(|c| {
            cube.domain_min[c] + grey * (cube.domain_max[c] - cube.domain_min[c])
        });
        cube.sample(input).map(|c| c.clamp(0.0, 1.0))
    })
}

/// Parses a `.cube` file and returns only its [`tone_ramp`].
pub fn load_ramp(bytes: &[u8]) -> Result<[[f32; 3]; RAMP.len()], LutError> {
    Ok(tone_ramp(&cube::parse(bytes)?))
}
