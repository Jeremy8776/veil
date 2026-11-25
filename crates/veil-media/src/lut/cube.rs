//! The `.cube` 3D LUT parser. A `.cube` file comes from outside Veil, so it is untrusted input.
//!
//! Purpose: [`parse`] turns the bytes of a `.cube` file into a [`Cube`], an RGB grid of
//! `size`^3 output colours, or a plain [`LutError`] that can be shown to the person.
//!
//! Invariants:
//! - Files over [`MAX_FILE`] bytes, lines over [`MAX_LINE`] bytes and grids over [`MAX_GRID`]
//!   points per side are refused before anything large is allocated.
//! - Only `TITLE`, `LUT_3D_SIZE`, `DOMAIN_MIN`, `DOMAIN_MAX` and `LUT_3D_INPUT_RANGE` are
//!   accepted, each once and before the first data line. Any other keyword is an error.
//! - Every number must be finite. Domain bounds and output values must lie within
//!   [`VALUE_RANGE`], and each domain minimum must be below its maximum.
//! - Exactly `size`^3 data lines, three numbers each, red changing fastest.
//! - Nothing here panics, and nothing is logged: the file's content stays local.

use std::fmt;

/// Largest `.cube` file accepted. A 65-point grid, the largest common size, is up to about
/// 12 MB with long decimals (real examples measured 11 MB).
pub const MAX_FILE: usize = 16 * 1024 * 1024;
/// Most grid points per side.
pub const MAX_GRID: usize = 65;
/// Fewest grid points per side: a cube needs two corners per axis.
pub const MIN_GRID: usize = 2;
/// Longest line accepted, in bytes. Real files use under 100.
pub const MAX_LINE: usize = 256;
/// Allowed range for domain bounds and output values. Film looks can overshoot 0 to 1
/// slightly; wider values belong to HDR LUTs, which a camera picture cannot use.
pub const VALUE_RANGE: (f32, f32) = (-1.0, 2.0);

/// Why a `.cube` file was refused. The text is plain enough to show as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LutError {
    /// Over [`MAX_FILE`].
    TooLarge,
    /// Not UTF-8 text.
    NotText,
    /// No `LUT_3D_SIZE` line.
    NoSize,
    /// A 1D LUT, which Veil does not use.
    OneDimensional,
    /// `LUT_3D_SIZE` outside [`MIN_GRID`]..=[`MAX_GRID`].
    BadSize,
    /// A line Veil cannot read. The number is the 1-based line.
    BadLine(usize),
    /// A number missing, not finite or out of range, on this line.
    BadValue(usize),
    /// A keyword given twice or after the data starts, on this line.
    Misplaced(usize),
    /// Fewer or more data lines than the size says.
    WrongCount,
    /// A domain minimum is not below its maximum.
    BadDomain,
}

impl fmt::Display for LutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "The file is too large for a LUT (over {} MB).", MAX_FILE / (1024 * 1024)),
            Self::NotText => f.write_str("The file is not a .cube text file."),
            Self::NoSize => f.write_str("The file has no LUT_3D_SIZE line, so it is not a 3D LUT."),
            Self::OneDimensional => f.write_str("1D LUTs are not supported. Use a 3D .cube LUT."),
            Self::BadSize => write!(f, "The LUT size must be between {MIN_GRID} and {MAX_GRID}."),
            Self::BadLine(n) => write!(f, "Line {n} is not valid in a .cube file."),
            Self::BadValue(n) => write!(f, "Line {n} has a missing or out-of-range number."),
            Self::Misplaced(n) => write!(f, "Line {n} repeats a setting or comes after the colours."),
            Self::WrongCount => f.write_str("The number of colours does not match the LUT size."),
            Self::BadDomain => f.write_str("The LUT's input range is empty or reversed."),
        }
    }
}

impl std::error::Error for LutError {}

/// A parsed 3D LUT: `size`^3 RGB outputs, red index fastest, then green, then blue.
#[derive(Clone, Debug, PartialEq)]
pub struct Cube {
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub table: Vec<[f32; 3]>,
}

impl Cube {
    /// The output at grid point (`r`, `g`, `b`). Callers keep each index below `size`.
    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        let i = (b * self.size + g) * self.size + r;
        self.table.get(i).copied().unwrap_or_default()
    }

    /// Trilinear sample at an RGB input in the cube's domain. Inputs outside are clamped.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let last = (self.size - 1) as f32;
        let mut idx = [0usize; 3];
        let mut frac = [0f32; 3];
        for c in 0..3 {
            let span = self.domain_max[c] - self.domain_min[c];
            let t = ((rgb[c] - self.domain_min[c]) / span).clamp(0.0, 1.0) * last;
            let i = (t.floor() as usize).min(self.size - 2);
            idx[c] = i;
            frac[c] = t - i as f32;
        }
        let [r, g, b] = idx;
        let mut out = [0f32; 3];
        for (corner, weight) in (0..8).map(|k| {
            let (dr, dg, db) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
            let w = pick(frac[0], dr) * pick(frac[1], dg) * pick(frac[2], db);
            (self.at(r + dr, g + dg, b + db), w)
        }) {
            for c in 0..3 {
                out[c] += corner[c] * weight;
            }
        }
        out
    }
}

fn pick(frac: f32, high: usize) -> f32 {
    if high == 1 { frac } else { 1.0 - frac }
}

/// Parses a `.cube` file. See the module invariants for what is refused.
pub fn parse(bytes: &[u8]) -> Result<Cube, LutError> {
    if bytes.len() > MAX_FILE {
        return Err(LutError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| LutError::NotText)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut size = None;
    let mut domain_min = None;
    let mut domain_max = None;
    let mut title = false;
    let mut table: Vec<[f32; 3]> = Vec::new();
    let mut expected = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        if raw.len() > MAX_LINE {
            return Err(LutError::BadLine(n));
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_ascii_whitespace();
        let first = words.next().unwrap_or_default();
        let is_data = first
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit() || b == b'-' || b == b'+' || b == b'.');
        if is_data {
            let Some(want) = size else {
                return Err(LutError::NoSize);
            };
            if expected == 0 {
                expected = want * want * want;
                table.reserve_exact(expected);
            }
            if table.len() >= expected {
                return Err(LutError::WrongCount);
            }
            table.push(three(first, &mut words, n)?);
            continue;
        }
        if !table.is_empty() {
            return Err(LutError::Misplaced(n));
        }
        match first {
            "TITLE" => {
                if std::mem::replace(&mut title, true) {
                    return Err(LutError::Misplaced(n));
                }
            }
            "LUT_1D_SIZE" => return Err(LutError::OneDimensional),
            "LUT_3D_SIZE" => {
                let value: usize = one_word(&mut words, n)?
                    .parse()
                    .map_err(|_| LutError::BadSize)?;
                if !(MIN_GRID..=MAX_GRID).contains(&value) {
                    return Err(LutError::BadSize);
                }
                if size.replace(value).is_some() {
                    return Err(LutError::Misplaced(n));
                }
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let w = one_word(&mut words, n)?;
                let v = three(w, &mut words, n)?;
                let slot = if first == "DOMAIN_MIN" {
                    &mut domain_min
                } else {
                    &mut domain_max
                };
                if slot.replace(v).is_some() {
                    return Err(LutError::Misplaced(n));
                }
            }
            "LUT_3D_INPUT_RANGE" => {
                let lo = number(Some(one_word(&mut words, n)?), n)?;
                let hi = number(words.next(), n)?;
                if words.next().is_some() {
                    return Err(LutError::BadLine(n));
                }
                if domain_min.replace([lo; 3]).is_some() || domain_max.replace([hi; 3]).is_some() {
                    return Err(LutError::Misplaced(n));
                }
            }
            _ => return Err(LutError::BadLine(n)),
        }
    }
    let size = size.ok_or(LutError::NoSize)?;
    if table.len() != size * size * size {
        return Err(LutError::WrongCount);
    }
    let domain_min = domain_min.unwrap_or([0.0; 3]);
    let domain_max = domain_max.unwrap_or([1.0; 3]);
    if (0..3).any(|c| domain_min[c] >= domain_max[c]) {
        return Err(LutError::BadDomain);
    }
    Ok(Cube {
        size,
        domain_min,
        domain_max,
        table,
    })
}

fn one_word<'a>(words: &mut impl Iterator<Item = &'a str>, n: usize) -> Result<&'a str, LutError> {
    words.next().ok_or(LutError::BadValue(n))
}

/// `first` and the next two words as numbers, with nothing after them.
fn three<'a>(
    first: &str,
    words: &mut impl Iterator<Item = &'a str>,
    n: usize,
) -> Result<[f32; 3], LutError> {
    let v = [
        number(Some(first), n)?,
        number(words.next(), n)?,
        number(words.next(), n)?,
    ];
    if words.next().is_some() {
        return Err(LutError::BadLine(n));
    }
    Ok(v)
}

/// A finite number within [`VALUE_RANGE`]. Rust's parser also takes `inf` and `NaN`; both
/// are refused here.
fn number(word: Option<&str>, n: usize) -> Result<f32, LutError> {
    let v: f32 = word
        .ok_or(LutError::BadValue(n))?
        .parse()
        .map_err(|_| LutError::BadValue(n))?;
    if v.is_finite() && (VALUE_RANGE.0..=VALUE_RANGE.1).contains(&v) {
        Ok(v)
    } else {
        Err(LutError::BadValue(n))
    }
}
