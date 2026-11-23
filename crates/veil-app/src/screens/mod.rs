//! One module per screen. Home and the end-of-call screen share `home.rs`; the call screen is
//! split into the stage and bottom bar (`call.rs`), the top bar (`call_top.rs`), the bar's
//! menus (`call_menus.rs`), the bar's device controls (`controls.rs`), the
//! stats overlay (`stats.rs`) and the side panels (`panels.rs`, with Security in
//! `security_panel.rs` and its group check in `group_check.rs`). Screen sharing's picker and
//! stage are `share.rs`, its floating bar `share_bar.rs`. Settings > Connection is
//! `connection.rs`.

pub(crate) mod bento;
pub mod call;
pub mod call_menus;
pub mod call_top;
pub mod chat_panel;
pub mod connection;
pub mod controls;
pub mod group_check;
pub mod home;
mod home_setup;
pub mod home_tools;
pub mod invite;
pub mod panels;
pub mod prejoin;
pub mod security_panel;
pub mod share;
pub mod share_bar;
pub mod stats;
