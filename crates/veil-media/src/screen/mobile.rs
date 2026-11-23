//! Mobile does not offer desktop screen capture. Camera and received screen video still work.
use crate::MediaError;
use crate::ladder::MAX_DIMENSION;
const MIN_SIZE: u32 = 16;
const UNAVAILABLE: MediaError =
    MediaError::Unavailable("screen sharing is not available in the mobile preview");
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenSource {
    Monitor(u32),
    Window(u32),
    /// A source picked in the desktop's chooser ([`choose_on_desktop`]), by pick number.
    Picked(u32),
}

/// Something that can be shared. `label` is a window's title: it stays on this device and
/// must not be logged (`log.rs` keeps names out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenChoice {
    pub source: ScreenSource,
    pub label: String,
}

/// The part of a source to share, as fractions of its width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// A part of a picture, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One captured picture: RGBA, 4 bytes a pixel, rows packed.
pub struct Grabbed {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn uses_system_chooser() -> bool {
    false
}
pub fn choose_on_desktop() -> Result<Option<ScreenSource>, MediaError> {
    Err(UNAVAILABLE)
}
pub fn release(_: ScreenSource) {}
pub fn hide_window_from_capture(_: &str) -> bool {
    false
}
pub fn set_main_window_hidden(_: &str, _: bool) -> bool {
    false
}
pub fn open_all(_: u32) -> Result<Vec<(ScreenChoice, ScreenCapture)>, MediaError> {
    Ok(Vec::new())
}
pub struct ScreenCapture;
impl ScreenCapture {
    pub fn open(_: ScreenSource) -> Result<Self, MediaError> {
        Err(UNAVAILABLE)
    }
    pub fn open_live(_: ScreenSource) -> Result<Self, MediaError> {
        Err(UNAVAILABLE)
    }
    pub fn open_thumbnail(_: ScreenSource) -> Result<Self, MediaError> {
        Err(UNAVAILABLE)
    }
    pub fn has_ended(&self) -> bool {
        true
    }
    pub fn grab(&self) -> Result<Option<Grabbed>, MediaError> {
        Err(UNAVAILABLE)
    }
}
/// The pixels `crop` covers in a `width` x `height` picture (all of it without a crop), with
/// even sizes. `None` if that leaves too little or too much to encode.
pub fn region(width: u32, height: u32, crop: Option<Crop>) -> Option<Region> {
    let edge = |fraction: f32, size: u32| {
        let at = (fraction.clamp(0.0, 1.0) * size as f32).round() as u32;
        at.min(size)
    };
    let (x0, y0, x1, y1) = crop.map_or((0, 0, width, height), |c| {
        (
            edge(c.left.min(c.right), width),
            edge(c.top.min(c.bottom), height),
            edge(c.left.max(c.right), width),
            edge(c.top.max(c.bottom), height),
        )
    });
    let (w, h) = ((x1 - x0) & !1, (y1 - y0) & !1);
    let limit = u32::from(MAX_DIMENSION);
    (w >= MIN_SIZE && h >= MIN_SIZE && w <= limit && h <= limit).then_some(Region {
        x: x0,
        y: y0,
        width: w,
        height: h,
    })
}

/// Converts `region` of `picture` to packed NV12 in `out`.
pub fn to_nv12(picture: &Grabbed, region: Region, out: &mut Vec<u8>) -> Result<(), MediaError> {
    let bad = MediaError::Malformed("screen picture");
    let (stride, w, h) = (
        picture.width as usize * 4,
        region.width as usize,
        region.height as usize,
    );
    let (x, y) = (region.x as usize, region.y as usize);
    let inside = region.x + region.width <= picture.width
        && region.y + region.height <= picture.height
        && picture.rgba.len() >= stride * picture.height as usize;
    if !inside || w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
        return Err(bad);
    }
    out.clear();
    out.reserve(w * h * 3 / 2);
    let row = |line: usize| &picture.rgba[(y + line) * stride + x * 4..][..w * 4];
    for line in 0..h {
        let (pixels, _) = row(line).as_chunks::<4>();
        out.extend(pixels.iter().map(|p| luma(p[0], p[1], p[2])));
    }
    for line in (0..h).step_by(2) {
        let (upper, _) = row(line).as_chunks::<8>();
        let (lower, _) = row(line + 1).as_chunks::<8>();
        for (a, b) in upper.iter().zip(lower) {
            // Each chroma sample is the average of its four pixels.
            let mean = |i: usize| {
                let sum =
                    u32::from(a[i]) + u32::from(a[i + 4]) + u32::from(b[i]) + u32::from(b[i + 4]);
                (sum / 4) as i32
            };
            let (r, g, b) = (mean(0), mean(1), mean(2));
            out.push(chroma(-26 * r - 86 * g + 112 * b));
            out.push(chroma(112 * r - 102 * g - 10 * b));
        }
    }
    Ok(())
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    let sum = 47 * u32::from(r) + 157 * u32::from(g) + 16 * u32::from(b);
    (16 + ((sum + 128) >> 8)) as u8
}

fn chroma(scaled: i32) -> u8 {
    (128 + ((scaled + 128) >> 8)).clamp(0, 255) as u8
}
