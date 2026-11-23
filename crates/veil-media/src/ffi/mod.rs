//! Native media code: one module per operating system, plus libopus on every platform and
//! OpenH264 off Windows. The only place `unsafe` is allowed in Veil. Each module is listed in
//! `docs/TRANSPARENCY.md` section 5.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "macos")]
pub mod macos;
// Also built for tests on Windows, so its unsafe code runs on the machine it is written on.
#[cfg(any(not(windows), test))]
pub mod openh264;
pub mod opus;
#[cfg(windows)]
pub mod windows;
