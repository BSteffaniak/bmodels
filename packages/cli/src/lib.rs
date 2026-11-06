#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! bmodels command-line interface.

use std::process::ExitCode;

use clap::Parser;

/// Watch model registries for new and changed AI models.
#[derive(Debug, Parser)]
#[command(name = "bmodels", version, about)]
struct Cli {}

/// Parses arguments and runs the requested command.
#[must_use]
pub fn run() -> ExitCode {
    let Cli {} = Cli::parse();
    ExitCode::SUCCESS
}
