//! Native media code: one module per operating system, plus libopus on every platform. The
//! only place `unsafe` is allowed in Veil. Each module is listed in `docs/TRANSPARENCY.md`
//! section 5.

pub mod opus;
#[cfg(windows)]
pub mod windows;
