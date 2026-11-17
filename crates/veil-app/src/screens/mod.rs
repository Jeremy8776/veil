//! One module per screen. Home and the end-of-call screen share `home.rs`; the call screen is
//! split into the stage and bars (`call.rs`), the bar's device controls (`controls.rs`), the
//! stats overlay (`stats.rs`) and the side panels (`panels.rs`). Settings >
//! Connection is `connection.rs`.

pub mod call;
pub mod connection;
pub mod controls;
pub mod home;
pub mod invite;
pub mod panels;
pub mod prejoin;
pub mod stats;
