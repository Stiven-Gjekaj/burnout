//! What a person types.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Writes a bootable USB drive from the command line.
#[derive(Debug, Parser)]
#[command(name = "burnout", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Set by Burnout when it starts itself again through `sudo`.
    ///
    /// This is a guard against asking for a password for ever, and not a way
    /// in. A person who types it only makes Burnout stop and say it needs
    /// root. It is an argument and not an environment variable because `sudo`
    /// clears the environment.
    #[arg(long, hide = true, global = true)]
    pub elevated: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show every drive that this computer reports.
    ///
    /// This needs no privilege. A person sees their drives before they give
    /// a password.
    List,

    /// Write an image to a drive.
    ///
    /// This erases the drive. Nothing undoes it.
    Write(WriteArgs),
}

#[derive(Debug, Args)]
pub struct WriteArgs {
    /// The image file to write.
    pub image: PathBuf,

    /// The number that `burnout list` printed beside the drive.
    ///
    /// There is no default. A person names the target every time.
    #[arg(required_unless_present = "device")]
    pub target: Option<usize>,

    /// Name the drive by its path instead of its number, for a script.
    #[arg(long, value_name = "PATH")]
    pub device: Option<String>,

    /// Allow a drive that Burnout cannot prove is removable.
    ///
    /// This never allows the drive that the system starts from.
    #[arg(long)]
    pub force: bool,

    /// Stop rather than ask for a password.
    #[arg(long)]
    pub no_elevate: bool,

    /// Choose the mode, and do not read the image to choose it.
    ///
    /// Burnout reads the image to choose the mode. It refuses a Windows ISO,
    /// because a byte copy of one starts nothing on most computers, and it
    /// refuses macOS media. `raw` copies the image byte for byte, whatever
    /// it holds.
    #[arg(long, value_enum, value_name = "MODE")]
    pub mode: Option<ModeArg>,
}

/// A mode that a person can choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    /// Copy the image byte for byte.
    Raw,
}
