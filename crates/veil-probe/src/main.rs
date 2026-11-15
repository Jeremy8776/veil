//! `veil-probe`: a two-party call with no camera, for testing links between real networks.
//!
//! Purpose: the host prints an invite; the joiner uses it. Both then exchange sealed probes on
//! the video and audio paths of a real `veil-call` session, so one run shows whether the
//! direct dial, the hole punch, the relay or the forwarder carried the call, and how well.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod args;
mod host;
mod join;
mod session;
mod wire;

use std::process::ExitCode;

use args::{Command, USAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let command = match args::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("veil-probe: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match command {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Host(args) => host::run(args).await,
        Command::Join(args) => join::run(args).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("veil-probe: {e}");
            ExitCode::FAILURE
        }
    }
}
