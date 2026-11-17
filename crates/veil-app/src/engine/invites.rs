//! How long the host's invite links work, and when the engine makes a fresh one.
//!
//! Invariants:
//! - Every link works once, whatever its lifetime. "Until the call ends" changes only how
//!   long an unused link stays valid.
//! - No link outlives the call: the lobby that accepts its token lives in the host's memory
//!   and ends with the call, whatever the expiry inside the link says.
//! - The link shown is never expired: one that runs out is replaced (`expired`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A link's lifetime by default.
const ONE_HOUR: Duration = Duration::from_secs(3_600);
/// "Until the call ends": longer than any call, since the call ending is what stops it.
const UNTIL_END: Duration = Duration::from_secs(30 * 24 * 3_600);

/// The expiry for a link made at `now_unix`.
pub fn expiry(until_end: bool, now_unix: u64) -> u64 {
    let lifetime = if until_end { UNTIL_END } else { ONE_HOUR };
    now_unix.saturating_add(lifetime.as_secs())
}

/// True when a link that expires at `expires` no longer works at `now_unix`.
pub fn expired(expires: u64, now_unix: u64) -> bool {
    now_unix >= expires
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_lasts_an_hour_or_until_the_call_ends() {
        assert_eq!(expiry(false, 1_000), 4_600);
        assert!(expiry(true, 1_000) > 1_000 + 7 * 24 * 3_600);
        assert_eq!(expiry(true, u64::MAX), u64::MAX, "no overflow");
    }

    #[test]
    fn a_link_stops_working_at_its_expiry() {
        assert!(!expired(4_600, 4_599));
        assert!(expired(4_600, 4_600));
    }
}
