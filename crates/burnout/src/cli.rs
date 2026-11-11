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
    /// `list` prints one object, which holds the drives. `info` prints one
    /// object, which describes the image. `write` prints one
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

    /// Show what an image is, and the mode that `write` uses for it.
    ///
    /// For a Windows ISO it lists the editions of the install image. It reads
    /// the image only, and needs no privilege.
    Info(InfoArgs),

    /// Write an image to a drive.
    ///
    /// This erases the drive. Nothing undoes it.
    Write(WriteArgs),
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// The image file to read.
    pub image: PathBuf,
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

    /// Write the image into this partition of the drive, and keep the others.
    ///
    /// The number is the one that the host shows: 2 for `sdb2`, `disk4s2` or
    /// partition 2 of Disk Management. Burnout reads it from the partition
    /// table of the drive, and shows the partition before it asks. The image
    /// goes in byte for byte, so a Windows ISO cannot go into a partition.
    #[arg(long, value_name = "NUMBER")]
    pub partition: Option<u32>,

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

    /// The edition that Setup installs, by its index or its name, as
    /// `burnout info` lists them.
    ///
    /// Windows mode only. Setup then does not ask for an edition. A name can
    /// be the end of the full name, such as Pro for Windows 11 Pro.
    #[arg(long, value_name = "EDITION")]
    pub edition: Option<String>,

    /// Make a drive that starts Windows itself: Windows To Go.
    ///
    /// Burnout applies the edition that `--edition` names to an NTFS
    /// partition, and puts the boot files and a BCD store on an EFI system
    /// partition. The drive starts through UEFI, with no Setup. The ISO must
    /// hold sources/install.wim.
    #[arg(
        long,
        requires = "edition",
        conflicts_with_all = ["partition", "mode", "skip_hardware_checks", "no_microsoft_account"]
    )]
    pub to_go: bool,
}

/// A mode that a person can choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    /// Copy the image byte for byte.
    Raw,
    /// Lay the drive out for Windows, and copy the files of the ISO onto it.
    Windows,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("burnout").chain(args.iter().copied()))
    }

    #[test]
    fn to_go_takes_an_edition() {
        let cli = parse(&["write", "w.iso", "2", "--to-go", "--edition", "Pro"]).unwrap();
        let Command::Write(w) = cli.command else {
            panic!("write")
        };
        assert!(w.to_go);
        assert_eq!(w.edition.as_deref(), Some("Pro"));
    }

    #[test]
    fn to_go_without_an_edition_is_refused() {
        let e = parse(&["write", "w.iso", "2", "--to-go"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn to_go_refuses_the_options_of_the_other_modes() {
        for extra in [
            &["--partition", "1"][..],
            &["--mode", "windows"],
            &["--skip-hardware-checks"],
            &["--no-microsoft-account"],
        ] {
            let mut args = vec!["write", "w.iso", "2", "--to-go", "--edition", "1"];
            args.extend_from_slice(extra);
            let e = parse(&args).unwrap_err();
            assert_eq!(
                e.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
    }
}
