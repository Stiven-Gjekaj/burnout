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

    /// Print JSON for a script to read, and not the text for a person.
    ///
    /// `list` prints one object, which holds the drives. `write` prints one
    /// object on a line for each event: the confirmation, the start, the
    /// progress and the end of each step, the result, an error, and a stop.
    /// The text for a person then goes to the error stream.
    #[arg(long, global = true)]
    pub json: bool,
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
    /// Burnout reads the image to choose the mode: a byte copy for a hybrid
    /// image, and the layout of Windows for a Windows ISO. It refuses macOS
    /// media, and an image that fits neither mode.
    #[arg(long, value_enum, value_name = "MODE")]
    pub mode: Option<ModeArg>,

    /// Turn off the checks of Windows 11 for TPM, Secure Boot, RAM, CPU and
    /// storage.
    ///
    /// Windows mode only. autounattend.xml then carries the LabConfig keys,
    /// and Setup installs on a machine that Windows 11 does not support.
    #[arg(long)]
    pub skip_hardware_checks: bool,

    /// Take away the step of Setup that asks for a Microsoft account.
    ///
    /// Windows mode only. Setup then asks for a local account.
    #[arg(long)]
    pub no_microsoft_account: bool,
}

/// A mode that a person can choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    /// Copy the image byte for byte.
    Raw,
    /// Lay the drive out for Windows, and copy the files of the ISO onto it.
    Windows,
}
