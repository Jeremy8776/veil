//! Native media APIs, one module per operating system. The only place `unsafe` is allowed in
//! Veil. Each module is listed in `docs/TRANSPARENCY.md` section 5.

#[cfg(windows)]
pub mod windows;
