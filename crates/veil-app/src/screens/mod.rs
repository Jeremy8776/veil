//! One module per screen. Home and the end-of-call screen share `home.rs`; the call screen is
//! split into the stage and bars (`call.rs`) and the side panels (`panels.rs`).

pub mod call;
pub mod home;
pub mod panels;
pub mod prejoin;
