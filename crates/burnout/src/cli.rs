//! What a person types.

use clap::{Parser, Subcommand};

/// Writes a bootable USB drive from the command line.
#[derive(Debug, Parser)]
#[command(name = "burnout", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show every drive that this computer reports.
    ///
    /// This needs no privilege. A person sees their drives before they give
    /// a password.
    List,
}
