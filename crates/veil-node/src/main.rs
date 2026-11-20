//! `veil-node [CONFIG]`: starts the roles in CONFIG (default `veil-node.toml`) and runs until
//! Ctrl+C. Prints each role's `key@address`, which is what goes into invites.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::path::PathBuf;
use std::process::ExitCode;

use veil_node::{Config, identity, start};

const USAGE: &str = "usage: veil-node [CONFIG]   (default: veil-node.toml)";

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = match (args.next(), args.next()) {
        (None, _) => PathBuf::from("veil-node.toml"),
        (Some(flag), _) if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        (Some(path), None) => PathBuf::from(path),
        (Some(_), Some(_)) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let running = match Config::load(&path)
        .and_then(|config| identity::load_or_create(&config.identity_file).map(|id| (config, id)))
        .and_then(|(config, id)| start(&config, &id))
    {
        Ok(running) => running,
        Err(e) => {
            eprintln!("veil-node: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(addr) = &running.rendezvous {
        println!("rendezvous  {addr}");
    }
    if let Some(addr) = &running.forwarder {
        println!("forwarder   {addr}");
    }
    match running.hosts {
        Some(n) => println!("hosts       {n} device key(s) may host; their guests may join"),
        None => println!("hosts       anyone may host (set hosts = [...] to choose who)"),
    }
    println!("Running. Press Ctrl+C to stop.");

    let code = match tokio::signal::ctrl_c().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("veil-node: cannot wait for Ctrl+C: {e}");
            ExitCode::FAILURE
        }
    };
    drop(running);
    code
}
