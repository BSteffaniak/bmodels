#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! bmodels command-line interface: argument parsing, command implementations, terminal output,
//! and the TUI backend that wires `bmodels_tui` to the store and engine.

mod args;
mod commands;
mod context;
mod output;
mod resolve;
mod tui_backend;

use std::process::ExitCode;

use clap::Parser;

pub use args::Cli;

/// Parses arguments and runs the requested command.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match commands::dispatch(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("bmodels: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        _ => log::LevelFilter::Debug,
    };
    let mut builder = pretty_env_logger::formatted_builder();
    builder.filter_level(level);
    if let Ok(filter) = std::env::var("RUST_LOG") {
        builder.parse_filters(&filter);
    }
    // Logging to stderr would corrupt the TUI; the TUI shows errors in its status bar.
    let _ = builder.try_init();
}
