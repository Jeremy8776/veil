//! One module per screen. Home and the end-of-call screen share `home.rs`; the call screen is
//! split into the stage and bottom bar (`call.rs`), the top bar (`call_top.rs`), the bar's
//! menus (`call_menus.rs`), the bar's device controls (`controls.rs`), the
//! stats overlay (`stats.rs`) and the side panels (`panels.rs`). Settings >
//! Connection is `connection.rs`.

pub mod call;
pub mod call_menus;
pub mod call_top;
pub mod connection;
pub mod controls;
pub mod home;
pub mod invite;
pub mod panels;
pub mod prejoin;
pub mod stats;
