//! The error that every operation in this crate returns.

use std::fmt;

use crate::{grouped, size_column};

/// What went wrong.
///
/// Each variant names one cause. A message says what the code can prove, and
/// it names the fix when a person can apply one.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The operating system refused a read, a write or a seek.
    Io(std::io::Error),
    /// Burnout does not support this operating system.
    Unsupported {
        /// The name of the target, as the compiler gives it.
        target: &'static str,
    },
    /// The host gave an answer that this code cannot read.
    Host {
        /// Where the answer came from, such as a file or a system call.
        source: String,
        /// What is wrong with the answer.
        detail: String,
    },
    /// No drive carries this name or this number.
    NoSuchDrive {
        /// The name or the number that the person gave.
        wanted: String,
    },
    /// A sector size that this code does not accept.
    SectorSize {
        /// The size that the host reported, in bytes.
        got: u32,
    },
    /// A length that the sector size does not divide.
    Unaligned {
        /// The length in bytes.
        length: u64,
        /// The sector size in bytes.
        sector_size: u32,
    },
    /// The image does not fit on the drive.
    TooSmall {
        /// The size of the image in bytes.
        image_bytes: u64,
        /// The size of the drive in bytes.
        drive_bytes: u64,
    },
    /// The drive is the one that the running system starts from.
    ///
    /// No option allows this. It is the one refusal that nothing overrides.
    SystemDisk {
        /// The drive, as the list names it.
        name: String,
    },
    /// The drive is not one that Burnout can prove is removable.
    NotRemovable {
        /// The drive, as the list names it.
        name: String,
    },
    /// The drive at that number is not the drive that the person saw.
    ///
    /// A number is a position in the list and not a name of a drive. Pull one
    /// drive out between the list and the write, and every later number moves
    /// to a different drive.
    DriveChanged {
        /// What the person asked for.
        wanted: String,
        /// What is there now.
        found: String,
    },
    /// The person did not confirm the drive.
    NotConfirmed {
        /// The drive, as a message names it.
        drive: String,
    },
    /// The drive holds something other than the image that went onto it.
    VerifyFailed {
        /// The digest of the source.
        expected: String,
        /// The digest of what the drive holds.
        got: String,
        /// Where the two first differ, when the check knows.
        ///
        /// A difference at the first byte and a difference near the end are
        /// different faults, and a person who has to choose a new drive is
        /// helped by knowing which one happened.
        at_byte: Option<u64>,
    },
    /// The image file cannot be read, or holds nothing to write.
    Image {
        /// The path, as the person gave it.
        path: String,
        /// What is wrong with it.
        detail: String,
    },
    /// The image is macOS media, which only macOS can make into a drive.
    MacosMedia {
        /// The path, as the person gave it.
        path: String,
        /// What the image is, such as "an installer package".
        what: &'static str,
    },
    /// An option of Windows mode came with an image that goes in raw mode.
    WindowsOption {
        /// The option, as a person types it.
        option: &'static str,
    },
    /// Neither mode fits the image: its first sector holds no boot table,
    /// and it holds no install image of Windows.
    NoMode {
        /// The path, as the person gave it.
        path: String,
    },
    /// The operation needs more privilege than this process holds.
    NeedsPrivilege {
        /// What a person types to get it.
        remedy: String,
    },
    /// A tree of files to copy holds a path that cannot be read or used.
    Source {
        /// The path. A path of the host when the tree is a directory of the
        /// host, and a path inside the tree otherwise.
        path: String,
        /// What is wrong with it.
        detail: String,
    },
    /// A file of the tree is larger than the file system can hold.
    FileTooLarge {
        /// The path inside the tree.
        path: String,
        /// The size of the file in bytes.
        bytes: u64,
        /// The file system, such as FAT32.
        file_system: &'static str,
        /// The largest file that the file system holds, in bytes.
        limit: u64,
    },
    /// A directory or a file of the tree cannot go onto the volume.
    CannotCopy {
        /// The path inside the tree.
        path: String,
        /// What is wrong.
        detail: String,
    },
    /// A volume does not hold what went onto it.
    VolumeDiffers {
        /// The path inside the tree where the two differ.
        path: String,
        /// How they differ.
        detail: String,
    },
    /// A partition is too small for the file system that has to go on it.
    PartitionTooSmall {
        /// The file system, such as FAT32.
        file_system: &'static str,
        /// The size of the partition in bytes.
        partition_bytes: u64,
        /// The smallest partition that the file system fits, in bytes.
        needed_bytes: u64,
    },
    /// The drive is too small for the layout of Windows mode.
    NeedsLargerDrive {
        /// The smallest drive that holds the layout, in bytes.
        needed_bytes: u64,
        /// The size of the drive in bytes.
        drive_bytes: u64,
    },
    /// The files of a tree do not fit on the volume that has to hold them.
    VolumeFull {
        /// The file system, such as exFAT.
        file_system: &'static str,
        /// What the tree takes on the volume, in whole clusters, in bytes.
        needed_bytes: u64,
        /// What the volume holds for files, in bytes.
        free_bytes: u64,
    },
}

/// Where a person reports a fault of Burnout.
const ISSUES: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/issues");

impl fmt::Display for Error {
    /// The message says what went wrong, and then what to do about it.
    ///
    /// A message starts in lower case and ends with no full stop, because
    /// the command line puts `burnout: ` before it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Unsupported { target } => {
                write!(
                    f,
                    "Burnout does not support {target}. Use Burnout on macOS, Linux or Windows"
                )
            }
            Error::Host { source, detail } => {
                write!(
                    f,
                    "{source} gave no answer that Burnout can use: {detail}. \
                     Report this message at {ISSUES}"
                )
            }
            Error::NoSuchDrive { wanted } => {
                write!(
                    f,
                    "Burnout finds no drive {wanted}. Run burnout list, and give the number that \
                     it shows beside the drive"
                )
            }
            Error::SectorSize { got } => {
                write!(
                    f,
                    "the drive has sectors of {got} bytes, and Burnout writes only sectors of 512 \
                     or 4096 bytes. Use a different drive"
                )
            }
            Error::Unaligned {
                length,
                sector_size,
            } => {
                write!(
                    f,
                    "a length of {} bytes is not a whole number of sectors of {sector_size} \
                     bytes. Report this message at {ISSUES}",
                    grouped(*length)
                )
            }
            Error::TooSmall {
                image_bytes,
                drive_bytes,
            } => {
                write!(
                    f,
                    "the image holds {}, and the drive holds {}. Use a larger drive",
                    size_column(*image_bytes),
                    size_column(*drive_bytes)
                )
            }
            Error::SystemDisk { name } => {
                write!(
                    f,
                    "{name} is the drive that this system starts from, and no option allows a \
                     write to it. Choose a different drive"
                )
            }
            Error::NotRemovable { name } => {
                write!(
                    f,
                    "Burnout cannot prove that {name} is removable. Use --force to write to it anyway"
                )
            }
            Error::DriveChanged { wanted, found } => {
                write!(
                    f,
                    "the drive changed after you confirmed it: it was {wanted}, and it is now \
                     {found}. Run burnout list again, and give the number of the drive"
                )
            }
            Error::NotConfirmed { drive } => {
                write!(
                    f,
                    "the answer does not match, so Burnout wrote nothing to {drive}"
                )
            }
            Error::VerifyFailed {
                expected,
                got,
                at_byte,
            } => {
                write!(f, "the drive does not hold the image")?;
                if let Some(at) = at_byte {
                    write!(
                        f,
                        ". The first byte that differs is at offset {}",
                        grouped(*at)
                    )?;
                }
                write!(
                    f,
                    ". The drive gives the SHA-256 {got}, and the image gives {expected}. {AGAIN}"
                )
            }
            Error::Image { path, detail } => {
                write!(f, "cannot read {path}: {detail}")
            }
            Error::MacosMedia { path, what } => {
                write!(
                    f,
                    "{path} is {what}, and only macOS can make a drive from macOS media. \
                     Use createinstallmedia, which is in Contents/Resources of the installer app"
                )
            }
            Error::WindowsOption { option } => {
                write!(
                    f,
                    "{option} changes only the autounattend.xml of Windows mode, and this image \
                     goes in raw mode. Remove {option}"
                )
            }
            Error::NoMode { path } => {
                write!(
                    f,
                    "{path} holds no boot table in its first sector and no install image of Windows, \
                     so a byte copy of it may start nothing. A compressed image is one of these, \
                     and it has to be expanded first. To make the byte copy anyway, use --mode raw"
                )
            }
            Error::NeedsPrivilege { remedy } => {
                write!(f, "Burnout needs more privilege to write a drive. {remedy}")
            }
            Error::Source { path, detail } => {
                write!(f, "cannot use {path}: {detail}")
            }
            Error::FileTooLarge {
                path,
                bytes,
                file_system,
                limit,
            } => {
                write!(
                    f,
                    "{path} holds {}, and {file_system} holds a file of {} bytes or fewer",
                    size_column(*bytes),
                    grouped(*limit)
                )
            }
            Error::CannotCopy { path, detail } => {
                write!(f, "cannot copy {path} onto the volume: {detail}")
            }
            Error::VolumeDiffers { path, detail } => {
                write!(
                    f,
                    "the drive does not hold what Burnout wrote in {path}: {detail}. {AGAIN}"
                )
            }
            Error::PartitionTooSmall {
                file_system,
                partition_bytes,
                needed_bytes,
            } => {
                write!(
                    f,
                    "{file_system} needs a partition of {} or more, and this one holds {}",
                    size_column(*needed_bytes),
                    size_column(*partition_bytes)
                )
            }
            Error::NeedsLargerDrive {
                needed_bytes,
                drive_bytes,
            } => {
                write!(
                    f,
                    "the layout of Windows mode needs a drive of {} or more, and this drive \
                     holds {}. Use a larger drive",
                    size_column(*needed_bytes),
                    size_column(*drive_bytes)
                )
            }
            Error::VolumeFull {
                file_system,
                needed_bytes,
                free_bytes,
            } => {
                write!(
                    f,
                    "the files take {} of the {file_system} volume, and it holds {} for files",
                    size_column(*needed_bytes),
                    size_column(*free_bytes)
                )
            }
        }
    }
}

/// The fix for a check that found a drive that does not hold what went onto
/// it. One more fault on a second write is a fault of the drive.
const AGAIN: &str = "Write the image again. If the check fails again, use a different drive";

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// The result of every operation in this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sector_size_message_gives_the_size_that_the_host_reported() {
        let e = Error::SectorSize { got: 1024 };
        assert_eq!(
            e.to_string(),
            "the drive has sectors of 1024 bytes, and Burnout writes only sectors of 512 or 4096 \
             bytes. Use a different drive"
        );
    }

    #[test]
    fn an_unaligned_message_gives_both_numbers_and_asks_for_a_report() {
        // Each length that this check sees comes from the host or from a
        // plan of Burnout. A length that fails it is a fault of Burnout.
        let text = Error::Unaligned {
            length: 1000,
            sector_size: 512,
        }
        .to_string();
        assert!(
            text.starts_with(
                "a length of 1,000 bytes is not a whole number of sectors of 512 bytes"
            ),
            "{text}"
        );
        assert!(text.ends_with(ISSUES), "{text}");
    }

    #[test]
    fn a_host_message_names_the_source_and_asks_for_a_report() {
        let e = Error::Host {
            source: "sysfs size".to_string(),
            detail: "\"abc\" is not a count of sectors".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "sysfs size gave no answer that Burnout can use: \"abc\" is not a count of sectors. \
             Report this message at https://github.com/Stiven-Gjekaj/burnout/issues"
        );
    }

    #[test]
    fn the_system_disk_message_says_that_no_option_allows_it() {
        // A message that only says no invites the reader to look for the
        // flag that turns it off. There is none.
        let e = Error::SystemDisk {
            name: "disk0".to_string(),
        };
        assert!(e.to_string().contains("no option"));
        assert!(e.to_string().ends_with("Choose a different drive"));
    }

    #[test]
    fn a_missing_drive_message_says_where_the_numbers_are() {
        let e = Error::NoSuchDrive {
            wanted: "7".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "Burnout finds no drive 7. Run burnout list, and give the number that it shows \
             beside the drive"
        );
    }

    #[test]
    fn a_fixed_drive_message_names_the_option_that_allows_it() {
        let e = Error::NotRemovable {
            name: "disk0".to_string(),
        };
        assert!(e.to_string().contains("--force"));
    }

    #[test]
    fn a_changed_drive_message_names_both_drives() {
        let e = Error::DriveChanged {
            wanted: "Samsung T7".to_string(),
            found: "SanDisk Ultra".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "the drive changed after you confirmed it: it was Samsung T7, and it is now SanDisk \
             Ultra. Run burnout list again, and give the number of the drive"
        );
    }

    #[test]
    fn a_declined_write_names_the_drive_that_it_left_alone() {
        let e = Error::NotConfirmed {
            drive: "Flash Drive (disk5, 128320801792 bytes)".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "the answer does not match, so Burnout wrote nothing to Flash Drive (disk5, \
             128320801792 bytes)"
        );
    }

    #[test]
    fn a_windows_option_message_names_the_option_to_remove() {
        let e = Error::WindowsOption {
            option: "--skip-hardware-checks",
        };
        let text = e.to_string();
        assert!(
            text.starts_with("--skip-hardware-checks changes only"),
            "{text}"
        );
        assert!(text.ends_with("Remove --skip-hardware-checks"), "{text}");
    }

    #[test]
    fn a_privilege_message_ends_with_the_remedy() {
        let e = Error::NeedsPrivilege {
            remedy: "Run the same command under sudo".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "Burnout needs more privilege to write a drive. Run the same command under sudo"
        );
    }

    #[test]
    fn an_unsupported_host_message_names_the_hosts_that_work() {
        let e = Error::Unsupported { target: "freebsd" };
        assert_eq!(
            e.to_string(),
            "Burnout does not support freebsd. Use Burnout on macOS, Linux or Windows"
        );
    }

    #[test]
    fn a_macos_message_names_what_the_image_is_and_createinstallmedia() {
        let e = Error::MacosMedia {
            path: "InstallAssistant.pkg".to_string(),
            what: "an installer package",
        };
        let text = e.to_string();
        assert!(
            text.starts_with("InstallAssistant.pkg is an installer package"),
            "{text}"
        );
        assert!(text.contains("createinstallmedia"), "{text}");
    }

    #[test]
    fn a_source_message_names_the_path_and_the_fault() {
        let e = Error::Source {
            path: "efi/boot".to_string(),
            detail: "permission denied".to_string(),
        };
        assert_eq!(e.to_string(), "cannot use efi/boot: permission denied");
    }

    #[test]
    fn a_large_file_message_gives_its_size_and_the_limit() {
        let e = Error::FileTooLarge {
            path: "sources/install.wim".to_string(),
            bytes: 5_000_000_000,
            file_system: "FAT32",
            limit: 4_294_967_295,
        };
        assert_eq!(
            e.to_string(),
            "sources/install.wim holds 5.0 GB (5,000,000,000 bytes), and FAT32 holds a file of \
             4,294,967,295 bytes or fewer"
        );
    }

    #[test]
    fn a_volume_message_names_the_path_where_the_two_differ() {
        let e = Error::VolumeDiffers {
            path: "efi/boot/bootx64.efi".to_string(),
            detail: "the volume holds 10 bytes and the source 12".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "the drive does not hold what Burnout wrote in efi/boot/bootx64.efi: the volume \
             holds 10 bytes and the source 12. Write the image again. If the check fails again, \
             use a different drive"
        );
    }

    #[test]
    fn a_partition_message_gives_the_size_it_has_and_the_size_it_needs() {
        let e = Error::PartitionTooSmall {
            file_system: "FAT32",
            partition_bytes: 1_048_576,
            needed_bytes: 34_077_184,
        };
        assert_eq!(
            e.to_string(),
            "FAT32 needs a partition of 34.1 MB (34,077,184 bytes) or more, and this one holds \
             1.0 MB (1,048,576 bytes)"
        );
    }

    #[test]
    fn a_larger_drive_message_gives_both_sizes_and_the_fix() {
        let e = Error::NeedsLargerDrive {
            needed_bytes: 7_000_000_000,
            drive_bytes: 4_000_000_000,
        };
        assert_eq!(
            e.to_string(),
            "the layout of Windows mode needs a drive of 7.0 GB (7,000,000,000 bytes) or more, \
             and this drive holds 4.0 GB (4,000,000,000 bytes). Use a larger drive"
        );
    }

    #[test]
    fn a_full_volume_message_gives_what_the_files_take_and_what_it_holds() {
        let e = Error::VolumeFull {
            file_system: "exFAT",
            needed_bytes: 7_000_000_000,
            free_bytes: 6_000_000_000,
        };
        assert_eq!(
            e.to_string(),
            "the files take 7.0 GB (7,000,000,000 bytes) of the exFAT volume, and it holds \
             6.0 GB (6,000,000,000 bytes) for files"
        );
    }

    #[test]
    fn a_failed_verification_message_gives_both_digests() {
        let e = Error::VerifyFailed {
            expected: "aa".to_string(),
            got: "bb".to_string(),
            at_byte: Some(4096),
        };
        assert_eq!(
            e.to_string(),
            "the drive does not hold the image. The first byte that differs is at offset 4,096. \
             The drive gives the SHA-256 bb, and the image gives aa. Write the image again. If \
             the check fails again, use a different drive"
        );
    }

    #[test]
    fn a_failed_verification_says_nothing_about_an_offset_it_does_not_know() {
        let e = Error::VerifyFailed {
            expected: "aa".to_string(),
            got: "bb".to_string(),
            at_byte: None,
        };
        assert!(!e.to_string().contains("differs"));
    }

    #[test]
    fn a_size_message_gives_both_sizes() {
        let e = Error::TooSmall {
            image_bytes: 8_000_000_000,
            drive_bytes: 4_000_000_000,
        };
        assert_eq!(
            e.to_string(),
            "the image holds 8.0 GB (8,000,000,000 bytes), and the drive holds 4.0 GB \
             (4,000,000,000 bytes). Use a larger drive"
        );
    }

    #[test]
    fn an_image_message_names_the_path_that_the_person_gave() {
        let e = Error::Image {
            path: "/tmp/ubuntu.iso".to_string(),
            detail: "No such file or directory".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "cannot read /tmp/ubuntu.iso: No such file or directory"
        );
    }

    /// One error of each kind.
    fn one_of_each() -> Vec<Error> {
        let text = || "x".to_string();
        let all = vec![
            Error::Io(std::io::Error::other("x")),
            Error::Unsupported { target: "x" },
            Error::Host {
                source: text(),
                detail: text(),
            },
            Error::NoSuchDrive { wanted: text() },
            Error::SectorSize { got: 1 },
            Error::Unaligned {
                length: 1,
                sector_size: 512,
            },
            Error::TooSmall {
                image_bytes: 2,
                drive_bytes: 1,
            },
            Error::SystemDisk { name: text() },
            Error::NotRemovable { name: text() },
            Error::DriveChanged {
                wanted: text(),
                found: text(),
            },
            Error::NotConfirmed { drive: text() },
            Error::VerifyFailed {
                expected: text(),
                got: text(),
                at_byte: Some(1),
            },
            Error::Image {
                path: text(),
                detail: text(),
            },
            Error::MacosMedia {
                path: text(),
                what: "x",
            },
            Error::WindowsOption { option: "x" },
            Error::NoMode { path: text() },
            Error::NeedsPrivilege { remedy: text() },
            Error::Source {
                path: text(),
                detail: text(),
            },
            Error::FileTooLarge {
                path: text(),
                bytes: 2,
                file_system: "x",
                limit: 1,
            },
            Error::CannotCopy {
                path: text(),
                detail: text(),
            },
            Error::VolumeDiffers {
                path: text(),
                detail: text(),
            },
            Error::PartitionTooSmall {
                file_system: "x",
                partition_bytes: 1,
                needed_bytes: 2,
            },
            Error::NeedsLargerDrive {
                needed_bytes: 2,
                drive_bytes: 1,
            },
            Error::VolumeFull {
                file_system: "x",
                needed_bytes: 2,
                free_bytes: 1,
            },
        ];
        // A new kind of error does not compile here until it is in the list
        // above, so it cannot miss the test below.
        for e in &all {
            match e {
                Error::Io(_)
                | Error::Unsupported { .. }
                | Error::Host { .. }
                | Error::NoSuchDrive { .. }
                | Error::SectorSize { .. }
                | Error::Unaligned { .. }
                | Error::TooSmall { .. }
                | Error::SystemDisk { .. }
                | Error::NotRemovable { .. }
                | Error::DriveChanged { .. }
                | Error::NotConfirmed { .. }
                | Error::VerifyFailed { .. }
                | Error::Image { .. }
                | Error::MacosMedia { .. }
                | Error::WindowsOption { .. }
                | Error::NoMode { .. }
                | Error::NeedsPrivilege { .. }
                | Error::Source { .. }
                | Error::FileTooLarge { .. }
                | Error::CannotCopy { .. }
                | Error::VolumeDiffers { .. }
                | Error::PartitionTooSmall { .. }
                | Error::NeedsLargerDrive { .. }
                | Error::VolumeFull { .. } => {}
            }
        }
        all
    }

    #[test]
    fn each_message_ends_with_no_full_stop_and_holds_no_dash_of_prose() {
        // The command line prints `burnout: ` and then the message. A full
        // stop at the end of one message and none at the end of the next
        // would read as two programs.
        for e in one_of_each() {
            let text = e.to_string();
            assert!(!text.ends_with('.'), "{text}");
            assert!(!text.contains(".."), "{text}");
            assert!(!text.contains('\u{2014}'), "{text}");
            assert!(!text.contains("  "), "{text}");
        }
    }

    #[test]
    fn an_input_output_error_keeps_the_error_under_it() {
        use std::error::Error as _;
        let e = Error::from(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert!(e.source().is_some());
    }
}
