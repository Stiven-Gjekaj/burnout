//! The command line of Burnout, and the three device layers under it.
//!
//! The binary holds almost nothing. The work lives here so that a test can
//! reach it, because a test cannot reach into a binary crate.

pub mod cli;
pub mod commands;
pub mod elevate;
pub mod format;
pub mod json;
pub mod platform;
pub mod report;
pub mod stop;

use clap::Parser;

/// Run what the person asked for, and give back the code to exit with.
pub fn run() -> i32 {
    let cli = cli::Cli::parse();
    let outcome = match &cli.command {
        cli::Command::List => commands::list::run(),
        cli::Command::Write(args) => commands::write::run(args, cli.elevated),
    };
    match outcome {
        Ok(code) => code,
        Err(e) => {
            eprintln!("burnout: {e}");
            if let Some(note) = stop::note(stop::reached(), &e) {
                eprintln!("{note}");
            }
            1
        }
    }
}
