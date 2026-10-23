//! `clockwork`: runs a DOS program on Clockwork.
//!
//! `run` is headless and prints a short summary (or JSON) meant for tools and agents.
//! `play` opens a window, takes keyboard input, and can record it as a session that `run
//! --replay` reproduces bit for bit.

mod gamepads;
pub mod gdb;
mod hotspots;
mod input_script;
mod listing;
mod live_audio;
mod options;
mod play;
mod run;
mod screenshot;
pub mod serve;
mod session;
mod summary;
mod unpack;

use std::process::ExitCode;

use crate::options::{USAGE, parse_options};

/// The command line.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match args.first().map(String::as_str) {
        Some("run") => parse_options(&args[1..]).and_then(|o| run::cmd_run(&o)),
        Some("play") => parse_options(&args[1..]).and_then(|o| play::run(&o)),
        Some("unpack") => unpack::cmd_unpack(&args[1..]),
        Some("listing") => listing::cmd_listing(&args[1..]),
        Some("serve") => serve::cmd_serve(&args[1..]),
        Some("ask") => serve::cmd_ask(&args[1..]),
        Some("gdb") => gdb::cmd_gdb(&args[1..]),
        Some("--version") => {
            println!("clockwork {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        }
    };
    result.unwrap_or_else(|message| {
        eprintln!("clockwork: error: {message}");
        ExitCode::from(1)
    })
}
