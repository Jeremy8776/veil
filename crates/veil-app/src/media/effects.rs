//! The camera thread's effects: mirror, Background blur, then one colour look (a built-in
//! Filter or an imported LUT), each blended with the original at its strength.
//!
//! Purpose: [`Chain::apply`] turns one captured NV12 picture into the picture that is shown
//! and sent. It keeps the camera thread's working state: the reused copy, the blur engine and
//! a LUT table blended for the current strength.
//!
//! Invariants:
//! - With no effect on (or every strength at 0%) the captured picture is returned as it is:
//!   not copied, and no blur worker exists.
//! - A LUT and a Filter are never both on: choosing one clears the other (`media.rs`).
//! - The blended LUT table is rebuilt only when the look or its strength changes; at 100% the
//!   look's own table is used.
//! - Effects run only here, on camera pictures, never on a screen share.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use veil_media::effects::{FULL, apply_filter, mirror};
use veil_media::{Blur, BlurEngine, BlurStatus, Filter, YuvLut};

use super::{LocalMedia, Shared, lock};
use crate::luts::ChosenLut;

/// Blur could not keep up; the picture must be neither shown nor sent.
pub(super) struct TooBusy;

/// The effect choices, read by the camera thread on its next picture.
impl LocalMedia {
    pub fn filter(&self) -> Filter {
        *lock(&self.shared.filter)
    }

    /// Takes effect on the next picture, for the preview and the call alike. Clears a LUT.
    pub fn set_filter(&self, filter: Filter) {
        *lock(&self.shared.filter) = filter;
        *lock(&self.shared.lut) = None;
    }

    /// The imported look in use, if any.
    pub fn lut(&self) -> Option<ChosenLut> {
        lock(&self.shared.lut).clone()
    }

    /// Uses an imported look instead of a Filter; `None` goes back to no filter.
    pub fn set_lut(&self, lut: Option<ChosenLut>) {
        *lock(&self.shared.filter) = Filter::None;
        *lock(&self.shared.lut) = lut;
    }

    /// The Filter or LUT strength, 0 to 100 percent.
    pub fn look_strength(&self) -> u8 {
        self.shared.look_strength.load(Ordering::Relaxed)
    }

    pub fn set_look_strength(&self, percent: u8) {
        self.shared
            .look_strength
            .store(percent.min(FULL), Ordering::Relaxed);
    }

    /// The Background blur strength, 0 to 100 percent.
    pub fn blur_strength(&self) -> u8 {
        self.shared.blur_strength.load(Ordering::Relaxed)
    }

    pub fn set_blur_strength(&self, percent: u8) {
        self.shared
            .blur_strength
            .store(percent.min(FULL), Ordering::Relaxed);
    }

    pub fn mirror(&self) -> bool {
        self.shared.mirror.load(Ordering::Relaxed)
    }

    /// Takes effect on the next picture, for the preview and the call alike.
    pub fn set_mirror(&self, mirror: bool) {
        self.shared.mirror.store(mirror, Ordering::Relaxed);
    }

    pub fn blur(&self) -> Blur {
        *lock(&self.shared.blur)
    }

    /// Takes effect on the next picture. Choosing blur again clears an old notice.
    pub fn set_blur(&self, blur: Blur) {
        *lock(&self.shared.blur) = blur;
        if blur != Blur::Off {
            *lock(&self.shared.blur_notice) = None;
        }
    }
}

#[derive(Default)]
pub(super) struct Chain {
    /// Changed copy of the picture; reused so effects cost no allocation per picture.
    filtered: Vec<u8>,
    /// Exists only while blur is on; dropping it stops the segmentation thread.
    blur: Option<BlurEngine>,
    /// The look and strength `mixed` was built for, and the blended table.
    mixed: Option<(Arc<YuvLut>, u8, YuvLut)>,
}

impl Chain {
    /// Applies the chosen effects to `data` (`w` x `h`, row pitch `stride`).
    pub(super) fn apply<'a>(
        &'a mut self,
        shared: &Shared,
        data: &'a [u8],
        w: usize,
        h: usize,
        stride: usize,
    ) -> Result<&'a [u8], TooBusy> {
        let filter = *lock(&shared.filter);
        let lut = lock(&shared.lut).as_ref().map(|c| Arc::clone(&c.lut));
        let look_strength = shared.look_strength.load(Ordering::Relaxed);
        let blur = *lock(&shared.blur);
        let blur_strength = shared.blur_strength.load(Ordering::Relaxed);
        let blur_on = blur != Blur::Off && blur_strength > 0;
        if !blur_on {
            self.blur = None;
        }
        let look_on = look_strength > 0 && (lut.is_some() || filter != Filter::None);
        if lut.is_none() {
            self.mixed = None;
        }
        let mirrored = shared.mirror.load(Ordering::Relaxed);
        if !mirrored && !look_on && !blur_on {
            return Ok(data);
        }
        self.filtered.clear();
        self.filtered.extend_from_slice(data);
        if mirrored {
            mirror(&mut self.filtered, w, h, stride);
        }
        if blur_on {
            let engine = self.blur.get_or_insert_with(BlurEngine::start);
            let status = engine.apply(&mut self.filtered, w, h, stride, blur, blur_strength);
            if status == BlurStatus::TooBusy {
                return Err(TooBusy);
            }
        }
        if look_on {
            match lut {
                Some(lut) if look_strength >= FULL => lut.apply(&mut self.filtered, w, h, stride),
                Some(lut) => {
                    let stale = self
                        .mixed
                        .as_ref()
                        .is_none_or(|(of, s, _)| !Arc::ptr_eq(of, &lut) || *s != look_strength);
                    if stale {
                        let table = lut.mixed(look_strength);
                        self.mixed = Some((lut, look_strength, table));
                    }
                    if let Some((_, _, table)) = &self.mixed {
                        table.apply(&mut self.filtered, w, h, stride);
                    }
                }
                None => apply_filter(&mut self.filtered, w, h, stride, filter, look_strength),
            }
        }
        Ok(&self.filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x2 NV12 picture: 8 luma bytes then 4 chroma bytes.
    fn picture() -> Vec<u8> {
        vec![60, 70, 80, 90, 100, 110, 120, 130, 90, 170, 90, 170]
    }

    fn shared() -> Shared {
        Shared {
            look_strength: FULL.into(),
            blur_strength: FULL.into(),
            ..Shared::default()
        }
    }

    fn run(shared: &Shared) -> Vec<u8> {
        let data = picture();
        let mut chain = Chain::default();
        match chain.apply(shared, &data, 4, 2, 4) {
            Ok(out) => out.to_vec(),
            Err(TooBusy) => Vec::new(),
        }
    }

    #[test]
    fn nothing_on_returns_the_picture_untouched() {
        assert_eq!(run(&shared()), picture());
        let s = shared();
        *lock(&s.filter) = Filter::Warm;
        s.look_strength.store(0, Ordering::Relaxed);
        assert_eq!(run(&s), picture(), "0% is the original");
    }

    #[test]
    fn a_filter_and_a_lut_follow_their_strength() {
        let s = shared();
        *lock(&s.filter) = Filter::BlackWhite;
        assert_eq!(&run(&s)[8..], &[128; 4], "100% is the whole filter");
        s.look_strength.store(50, Ordering::Relaxed);
        assert_eq!(&run(&s)[8..], &[109, 149, 109, 149], "half way");

        let s = shared();
        let lut = Arc::new(YuvLut::identity().mixed(0));
        *lock(&s.lut) = Some(ChosenLut {
            name: String::from("Same"),
            lut,
        });
        assert_eq!(run(&s), picture(), "an identity look changes nothing");
        s.look_strength.store(40, Ordering::Relaxed);
        assert_eq!(run(&s), picture());
    }
}
