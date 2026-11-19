//! Which video layers to ask the forwarder for, from the size of each person's tile.
//!
//! Purpose: through a forwarder, every receiver would otherwise get every layer of every
//! sender. The call screen reports each other person's tile width in physical pixels
//! ([`super::Engine::watch`]); [`request`] turns that into one `Wanted` per device, which the
//! session sends with `MediaSession::subscribe`.
//!
//! Invariants:
//! - A tile no wider than [`SMALL_TILE_PX`] gets the lowest spatial layer at every frame rate
//!   the sender sends; anything wider gets every layer.
//! - Someone with no tile on screen gets the lowest layer, never nothing, so their tile has
//!   a picture the moment it appears.
//! - The request is sorted by device, so an unchanged screen gives an equal request and the
//!   session sends nothing new.
//!
//! shortcut: two sizes only (the camera ladder's Thumb, or everything). Add a middle class
//! if a 3-rung ladder (Uhd) is sent in calls. When a tile shrinks, `video_in.rs` holds the
//! last large picture for about a second before it moves to the small layer; tell it
//! directly if that freeze shows.

use std::collections::HashMap;

use veil_forward::Wanted;
use veil_security::PublicId;

/// Widest tile, in physical pixels, that the Thumb layer (640 wide) fills well.
pub const SMALL_TILE_PX: f32 = 720.0;

/// The lowest spatial layer, at every frame rate.
pub const SMALL: Wanted = Wanted {
    spatial: 0,
    temporal: Wanted::ALL.temporal,
};

pub fn for_width(px: f32) -> Wanted {
    if px <= SMALL_TILE_PX {
        SMALL
    } else {
        Wanted::ALL
    }
}

/// One entry per other member: `others` as (participant id, device), `tiles` as
/// (participant id, tile width in physical pixels). `cap`, from the data use choice, is the
/// most asked of anyone.
pub fn request(
    others: &[(u32, PublicId)],
    tiles: &[(u32, f32)],
    cap: Option<Wanted>,
) -> Vec<(PublicId, Wanted)> {
    let widths: HashMap<u32, f32> = tiles.iter().copied().collect();
    let mut request: Vec<(PublicId, Wanted)> = others
        .iter()
        .map(|(id, device)| {
            let wanted = widths.get(id).map_or(SMALL, |w| for_width(*w));
            (*device, cap.map_or(wanted, |cap| wanted.min(cap)))
        })
        .collect();
    request.sort_by_key(|(device, _)| device.0);
    request
}

#[cfg(test)]
mod tests {
    use veil_security::Identity;

    use super::*;

    #[test]
    fn small_tiles_get_the_lowest_layer_and_large_ones_everything() {
        assert_eq!(for_width(320.0), SMALL);
        assert_eq!(for_width(SMALL_TILE_PX), SMALL);
        assert_eq!(for_width(1200.0), Wanted::ALL);
    }

    #[test]
    fn people_without_a_tile_get_the_lowest_layer_and_order_is_stable() {
        let (a, b, c) = (
            Identity::generate().public(),
            Identity::generate().public(),
            Identity::generate().public(),
        );
        let others = [(1, a), (2, b), (3, c)];
        let tiles = [(1, 1600.0), (2, 200.0)];
        let first = request(&others, &tiles, None);
        let reversed = [(3, c), (2, b), (1, a)];
        assert_eq!(first, request(&reversed, &tiles, None));
        let wanted: HashMap<PublicId, Wanted> = first.into_iter().collect();
        assert_eq!(wanted[&a], Wanted::ALL);
        assert_eq!(wanted[&b], SMALL);
        assert_eq!(wanted[&c], SMALL);
    }

    #[test]
    fn a_cap_lowers_every_request() {
        let (a, b) = (Identity::generate().public(), Identity::generate().public());
        let others = [(1, a), (2, b)];
        let tiles = [(1, 1600.0), (2, 200.0)];
        let saver: HashMap<PublicId, Wanted> =
            request(&others, &tiles, Some(SMALL)).into_iter().collect();
        assert_eq!(saver[&a], SMALL);
        assert_eq!(saver[&b], SMALL);
        let thumb: HashMap<PublicId, Wanted> =
            request(&others, &tiles, Some(Wanted::THUMB)).into_iter().collect();
        assert_eq!(thumb[&a], Wanted::THUMB);
        assert_eq!(thumb[&b], Wanted::THUMB);
    }
}
