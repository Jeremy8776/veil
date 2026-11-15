//! Reads just enough of an H.264 Annex B bitstream to check what an encoder produced.
//!
//! Purpose: the encoder is told to make a keyframe and to follow the plan's temporal layers.
//! These helpers let [`crate::simulcast`] check that it did, without trusting the platform
//! encoder's word for it.
//!
//! Invariants:
//! - Never panics on any input; a truncated or odd bitstream just yields fewer units.
//! - Only the NAL unit header (and the SVC extension of a prefix unit) is read.

/// NAL unit type of a coded slice of an IDR picture.
pub const NAL_IDR: u8 = 5;
/// NAL unit type of a coded slice of a non-IDR picture.
pub const NAL_SLICE: u8 = 1;
/// NAL unit type of a prefix unit, which carries the temporal id for temporal layering.
pub const NAL_PREFIX: u8 = 14;

/// Every NAL unit in `stream`, without its start code.
pub fn units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(stream.len(), |next| next - 3);
        // A four-byte start code leaves one more zero before the next unit.
        while end > start && stream[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            out.push(&stream[start..end]);
        }
    }
    out
}

/// The NAL unit types in `stream`, in order.
pub fn unit_types(stream: &[u8]) -> Vec<u8> {
    units(stream).iter().map(|unit| unit[0] & 0x1f).collect()
}

/// True if `stream` holds a slice of an IDR picture.
pub fn has_idr(stream: &[u8]) -> bool {
    unit_types(stream).contains(&NAL_IDR)
}

/// The temporal id from the first prefix unit, if the encoder wrote one.
pub fn temporal_id(stream: &[u8]) -> Option<u8> {
    units(stream)
        .into_iter()
        .find(|unit| unit[0] & 0x1f == NAL_PREFIX)
        .and_then(|unit| unit.get(3))
        .map(|byte| byte >> 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_units_after_three_and_four_byte_start_codes() {
        let stream = [0, 0, 0, 1, 0x67, 9, 0, 0, 1, 0x68, 8, 0, 0, 0, 1, 0x65, 7];
        assert_eq!(units(&stream), [&[0x67, 9][..], &[0x68, 8], &[0x65, 7]]);
        assert_eq!(unit_types(&stream), [7, 8, 5]);
        assert!(has_idr(&stream));
    }

    #[test]
    fn reads_the_prefix_temporal_id() {
        // Prefix unit: header, then the SVC extension with temporal id 2 in the top bits.
        let stream = [0, 0, 1, 0x6e, 0x80, 0x00, 0x40, 0, 0, 1, 0x21, 1];
        assert_eq!(temporal_id(&stream), Some(2));
        assert!(!has_idr(&stream));
        assert_eq!(temporal_id(&[0, 0, 1, 0x21, 1]), None);
    }

    #[test]
    fn odd_input_does_not_panic() {
        for stream in [&[][..], &[0], &[0, 0, 1], &[0, 0, 1, 0x6e], &[0, 0, 0, 0]] {
            let _ = (units(stream), temporal_id(stream), has_idr(stream));
        }
    }
}
