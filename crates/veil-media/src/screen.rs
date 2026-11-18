//! Screen capture: a whole screen, one window, or part of either, as NV12 for the encoder.
//!
//! Purpose: lists what can be shared ([`open_all`]), grabs its pictures ([`ScreenCapture`]) and
//! turns them into the packed NV12 the video encoders take. The `xcap` crate does the
//! operating system's part: GDI on Windows, Core Graphics on macOS, and X11 or the desktop
//! portal on Linux. A live share on Windows uses a picture stream instead
//! ([`ScreenCapture::open_live`]).
//!
//! Invariants:
//! - A source is only ever captured by the id the person chose. If it goes away the capture
//!   fails; it never falls back to another window or screen.
//! - A window is captured whether or not it is focused or covered. A minimised one has no
//!   picture, which [`ScreenCapture::grab`] reports as `None`, not as a failure.
//! - This process's own windows are not offered, so sharing Veil's window cannot show itself.
//! - A crop is kept as fractions of the source, so it still fits after a window is resized.
//! - Output sizes are even and at most [`MAX_DIMENSION`], as 4:2:0 and the ladder need.
//! - Colour is BT.709 limited range, the same as cameras and `veil-app`'s `convert.rs`.
//! - Nothing is captured in a Wayland session. There `xcap` takes each picture by having the
//!   desktop write a PNG file, which would put your screen on disk many times a second.
//!
//! shortcut: off Windows each live picture is still a full screenshot, which limits the rate.
//! Use each system's capture stream there once sharing has been tested on it. On Wayland that
//! stream (the PipeWire screen cast portal) is also what would lift the block.

use crate::MediaError;
#[cfg(target_os = "macos")]
use crate::ffi::macos;
#[cfg(windows)]
use crate::ffi::windows::{ScreenStream, is_app_window};
use crate::ladder::MAX_DIMENSION;

/// The smallest picture worth sending, in pixels each way.
const MIN_SIZE: u32 = 16;
const UNAVAILABLE: MediaError = MediaError::Unavailable("screen capture");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenSource {
    Monitor(u32),
    Window(u32),
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

/// shortcut: only Windows tells overlays from applications here. Add a rule for macOS and
/// Linux if their window lists show the same clutter.
#[cfg(not(windows))]
fn is_app_window(_id: u32) -> bool {
    true
}

/// Why this session cannot share its screen at all, in words for the person using it.
pub fn blocked_reason() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        let var = |name| std::env::var_os(name).unwrap_or_default();
        let (session, display) = (var("XDG_SESSION_TYPE"), var("WAYLAND_DISPLAY"));
        if is_wayland(&session.to_string_lossy(), &display.to_string_lossy()) {
            return Some("Screen sharing needs an X11 session for now.");
        }
    }
    None
}

/// The same test `xcap` uses to choose its Wayland path.
#[cfg(any(target_os = "linux", test))]
fn is_wayland(session_type: &str, display: &str) -> bool {
    session_type == "wayland" || display.to_lowercase().contains("wayland")
}

fn check_allowed() -> Result<(), MediaError> {
    if blocked_reason().is_some() {
        return Err(UNAVAILABLE);
    }
    #[cfg(target_os = "macos")]
    if !macos::may_capture_screen() {
        return Err(UNAVAILABLE);
    }
    Ok(())
}

/// True for a window that still exists but has no picture now. On macOS `xcap` loses sight of
/// a window that is minimised or on another desktop, and reports that as an error.
#[cfg(target_os = "macos")]
fn is_out_of_sight(window: &xcap::Window) -> bool {
    window.is_minimized().is_err() && window.id().is_ok_and(macos::window_exists)
}

#[cfg(not(target_os = "macos"))]
fn is_out_of_sight(_window: &xcap::Window) -> bool {
    false
}

/// Keeps this app's window called `title` out of screen captures. True once done.
#[cfg(windows)]
pub fn hide_window_from_capture(title: &str) -> bool {
    crate::ffi::windows::hide_from_capture(title)
}

/// shortcut: nothing is hidden here yet, so the bar shows in a whole-screen share. Add each
/// system's call once sharing has been tested on it.
#[cfg(not(windows))]
pub fn hide_window_from_capture(_title: &str) -> bool {
    true
}

/// Everything that can be shared, each already open: screens first, the main one at the top,
/// then windows from front to back. Leaves out minimised, empty and untitled windows, overlays
/// and helper windows, and those of process `own_pid`.
pub fn open_all(own_pid: u32) -> Result<Vec<(ScreenChoice, ScreenCapture)>, MediaError> {
    check_allowed()?;
    let mut monitors = xcap::Monitor::all().map_err(|_| UNAVAILABLE)?;
    monitors.sort_by_key(|m| !m.is_primary().unwrap_or(false));
    let mut list = Vec::new();
    for (index, monitor) in monitors.into_iter().enumerate() {
        if let Ok(id) = monitor.id() {
            let choice = ScreenChoice {
                source: ScreenSource::Monitor(id),
                label: format!("Screen {}", index + 1),
            };
            let capture = ScreenCapture {
                target: Target::Monitor(monitor),
            };
            list.push((choice, capture));
        }
    }
    // Screens can still be shared on a system that cannot list windows.
    for window in xcap::Window::all().unwrap_or_default() {
        let shown = window.pid().is_ok_and(|pid| pid != own_pid)
            && !window.is_minimized().unwrap_or(true)
            && window.width().unwrap_or(0) >= MIN_SIZE
            && window.height().unwrap_or(0) >= MIN_SIZE;
        let title = window.title().unwrap_or_default();
        let id = window.id().ok().filter(|&id| is_app_window(id));
        if let (true, false, Some(id)) = (shown, title.trim().is_empty(), id) {
            let choice = ScreenChoice {
                source: ScreenSource::Window(id),
                label: title,
            };
            let capture = ScreenCapture {
                target: Target::Window(window),
            };
            list.push((choice, capture));
        }
    }
    Ok(list)
}

enum Target {
    Monitor(xcap::Monitor),
    Window(xcap::Window),
    #[cfg(windows)]
    Stream(ScreenStream),
}

/// An open source. It holds system handles, so it stays on the thread that opened it.
pub struct ScreenCapture {
    target: Target,
}

impl ScreenCapture {
    pub fn open(source: ScreenSource) -> Result<Self, MediaError> {
        check_allowed()?;
        let target = match source {
            ScreenSource::Monitor(id) => xcap::Monitor::all()
                .map_err(|_| UNAVAILABLE)?
                .into_iter()
                .find(|m| m.id().is_ok_and(|found| found == id))
                .map(Target::Monitor),
            ScreenSource::Window(id) => xcap::Window::all()
                .map_err(|_| UNAVAILABLE)?
                .into_iter()
                .find(|w| w.id().is_ok_and(|found| found == id))
                .map(Target::Window),
        };
        target.map(|target| Self { target }).ok_or(UNAVAILABLE)
    }

    /// Opens `source` for a live share: a picture stream where there is one, otherwise the
    /// same screenshots as [`Self::open`].
    pub fn open_live(source: ScreenSource) -> Result<Self, MediaError> {
        #[cfg(windows)]
        if let Ok(stream) = ScreenStream::open(source) {
            return Ok(Self {
                target: Target::Stream(stream),
            });
        }
        Self::open(source)
    }

    /// The source's picture now. `None` means there is no new one: a minimised window, or on
    /// a live stream a source that has not changed.
    pub fn grab(&self) -> Result<Option<Grabbed>, MediaError> {
        let image = match &self.target {
            #[cfg(windows)]
            Target::Stream(stream) => return stream.next(),
            Target::Monitor(monitor) => monitor.capture_image(),
            Target::Window(window)
                if window.is_minimized().unwrap_or(false) || is_out_of_sight(window) =>
            {
                return Ok(None);
            }
            Target::Window(window) => window.capture_image(),
        }
        .map_err(|_| UNAVAILABLE)?;
        Ok(Some(Grabbed {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        }))
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
                let sum = u32::from(a[i]) + u32::from(a[i + 4]) + u32::from(b[i]) + u32::from(b[i + 4]);
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Grabbed {
        let pixel = [rgb[0], rgb[1], rgb[2], 255];
        Grabbed {
            width,
            height,
            rgba: pixel.repeat((width * height) as usize),
        }
    }

    fn all(width: u32, height: u32) -> Region {
        Region {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// Reads this computer's real screen into memory; nothing is kept or sent.
    #[test]
    #[ignore = "captures the screen"]
    fn the_first_screen_can_be_captured_and_converted() {
        let list = open_all(std::process::id()).unwrap();
        println!("{} sources", list.len());
        let picture = list[0].1.grab().unwrap().unwrap();
        let area = region(picture.width, picture.height, None).unwrap();
        let mut out = Vec::new();
        to_nv12(&picture, area, &mut out).unwrap();
        let (w, h) = (area.width as usize, area.height as usize);
        assert_eq!(out.len(), w * h * 3 / 2);
    }

    #[test]
    fn a_wayland_session_is_recognised() {
        assert!(is_wayland("wayland", ""));
        assert!(is_wayland("", "wayland-0"));
        assert!(!is_wayland("x11", ""));
        assert!(!is_wayland("", ""));
    }

    #[test]
    fn white_black_and_red_get_the_expected_colours() {
        let mut out = Vec::new();
        to_nv12(&solid(4, 2, [255, 255, 255]), all(4, 2), &mut out).unwrap();
        assert_eq!(out.len(), 12);
        assert_eq!((out[0], out[8], out[9]), (235, 128, 128));
        to_nv12(&solid(4, 2, [0, 0, 0]), all(4, 2), &mut out).unwrap();
        assert_eq!((out[0], out[8], out[9]), (16, 128, 128));
        to_nv12(&solid(4, 2, [255, 0, 0]), all(4, 2), &mut out).unwrap();
        assert_eq!((out[0], out[8], out[9]), (63, 102, 240));
    }

    #[test]
    fn a_region_reads_only_its_own_pixels() {
        let mut picture = solid(4, 4, [0, 0, 0]);
        // The bottom right quarter is white.
        for y in 2..4 {
            for x in 2..4 {
                picture.rgba[(y * 4 + x) * 4..][..3].fill(255);
            }
        }
        let mut out = Vec::new();
        let corner = Region {
            x: 2,
            y: 2,
            width: 2,
            height: 2,
        };
        to_nv12(&picture, corner, &mut out).unwrap();
        assert_eq!(out, [235, 235, 235, 235, 128, 128]);
    }

    #[test]
    fn pictures_and_regions_that_do_not_fit_are_refused() {
        let mut out = Vec::new();
        let picture = solid(4, 4, [0, 0, 0]);
        assert!(to_nv12(&picture, all(6, 4), &mut out).is_err(), "too wide");
        assert!(to_nv12(&picture, all(3, 4), &mut out).is_err(), "odd");
        let short = Grabbed {
            rgba: vec![0; 8],
            ..solid(4, 4, [0, 0, 0])
        };
        assert!(to_nv12(&short, all(4, 4), &mut out).is_err(), "short buffer");
    }

    #[test]
    fn a_crop_follows_the_source_and_stays_even() {
        let half = Crop {
            left: 0.5,
            top: 0.0,
            right: 1.0,
            bottom: 0.5,
        };
        assert_eq!(
            region(1_920, 1_080, Some(half)),
            Some(Region {
                x: 960,
                y: 0,
                width: 960,
                height: 540
            })
        );
        assert_eq!(region(1_000, 500, Some(half)).unwrap().width, 500);
        assert_eq!(region(1_921, 1_081, None).unwrap().width, 1_920);
    }

    #[test]
    fn a_crop_dragged_backwards_counts_and_a_tiny_one_does_not() {
        let backwards = Crop {
            left: 1.0,
            top: 1.0,
            right: 0.5,
            bottom: 0.5,
        };
        assert_eq!(region(100, 100, Some(backwards)).unwrap().x, 50);
        let tiny = Crop {
            left: 0.5,
            top: 0.5,
            right: 0.501,
            bottom: 0.501,
        };
        assert_eq!(region(100, 100, Some(tiny)), None);
        assert_eq!(region(u32::from(MAX_DIMENSION) + 2, 100, None), None);
    }
}
