//! One module per screen. Home and the end-of-call screen share `home.rs`; the call screen is
//! split into the stage and bars (`call.rs`) and the side panels (`panels.rs`). Settings >
//! Connection is `connection.rs`.

pub mod call;
pub mod connection;
pub mod home;
pub mod panels;
pub mod prejoin;
