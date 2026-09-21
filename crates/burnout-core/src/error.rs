//! The error that every operation in this crate returns.

use std::fmt;

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
    NotConfirmed,
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
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Unsupported { target } => {
                write!(f, "Burnout does not support {target}")
            }
            Error::Host { source, detail } => {
                write!(
                    f,
                    "{source} gave an answer that Burnout cannot read: {detail}"
                )
            }
            Error::NoSuchDrive { wanted } => {
                write!(f, "no drive carries the name {wanted}")
            }
            Error::SectorSize { got } => {
                write!(f, "a sector size of {got} bytes is not 512 and not 4096")
            }
            Error::Unaligned {
                length,
                sector_size,
            } => {
                write!(
                    f,
                    "a length of {length} bytes does not divide by a sector of {sector_size} bytes"
                )
            }
            Error::TooSmall {
                image_bytes,
                drive_bytes,
            } => {
                write!(
                    f,
                    "the image holds {image_bytes} bytes and the drive holds {drive_bytes}"
                )
            }
            Error::SystemDisk { name } => {
                write!(
                    f,
                    "{name} is the drive that this system starts from, and no option allows a write to it"
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
                    "that number named {wanted} and it now names {found}. Run burnout list again"
                )
            }
            Error::NotConfirmed => {
                write!(f, "the drive was not confirmed, so nothing was written")
            }
            Error::VerifyFailed {
                expected,
                got,
                at_byte,
            } => {
                write!(
                    f,
                    "the drive holds {got} and the image holds {expected}, so the write did not arrive"
                )?;
                match at_byte {
                    Some(at) => write!(f, ". The first byte that differs is at {at}"),
                    None => Ok(()),
                }
            }
            Error::Image { path, detail } => {
                write!(f, "cannot read {path}: {detail}")
            }
            Error::NeedsPrivilege { remedy } => {
                write!(f, "this needs more privilege than it has. {remedy}")
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
                    "{path} holds {bytes} bytes, and {file_system} holds a file of {limit} bytes or fewer"
                )
            }
            Error::CannotCopy { path, detail } => {
                write!(f, "cannot copy {path} onto the volume: {detail}")
            }
            Error::VolumeDiffers { path, detail } => {
                write!(f, "the volume and the source differ at {path}: {detail}")
            }
            Error::PartitionTooSmall {
                file_system,
                partition_bytes,
                needed_bytes,
            } => {
                write!(
                    f,
                    "{file_system} needs a partition of {needed_bytes} bytes or more, and this one holds {partition_bytes}"
                )
            }
        }
    }
}

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
            "a sector size of 1024 bytes is not 512 and not 4096"
        );
    }

    #[test]
    fn an_unaligned_message_gives_both_numbers() {
        let e = Error::Unaligned {
            length: 1000,
            sector_size: 512,
        };
        assert!(e.to_string().contains("1000"));
        assert!(e.to_string().contains("512"));
    }

    #[test]
    fn the_system_disk_message_says_that_no_option_allows_it() {
        // A message that only says no invites the reader to look for the
        // flag that turns it off. There is none.
        let e = Error::SystemDisk {
            name: "disk0".to_string(),
        };
        assert!(e.to_string().contains("no option"));
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
        let text = e.to_string();
        assert!(text.contains("Samsung T7"));
        assert!(text.contains("SanDisk Ultra"));
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
        let text = e.to_string();
        assert!(text.contains("sources/install.wim"));
        assert!(text.contains("5000000000"));
        assert!(text.contains("4294967295"));
    }

    #[test]
    fn a_volume_message_names_the_path_where_the_two_differ() {
        let e = Error::VolumeDiffers {
            path: "efi/boot/bootx64.efi".to_string(),
            detail: "the volume holds 10 bytes and the source 12".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "the volume and the source differ at efi/boot/bootx64.efi: the volume holds 10 bytes and the source 12"
        );
    }

    #[test]
    fn a_partition_message_gives_the_size_it_has_and_the_size_it_needs() {
        let e = Error::PartitionTooSmall {
            file_system: "FAT32",
            partition_bytes: 1_048_576,
            needed_bytes: 34_077_184,
        };
        let text = e.to_string();
        assert!(text.contains("FAT32"));
        assert!(text.contains("1048576"));
        assert!(text.contains("34077184"));
    }

    #[test]
    fn a_failed_verification_message_gives_both_digests() {
        let e = Error::VerifyFailed {
            expected: "aa".to_string(),
            got: "bb".to_string(),
            at_byte: Some(4096),
        };
        let text = e.to_string();
        assert!(text.contains("aa"));
        assert!(text.contains("bb"));
        assert!(text.ends_with("at 4096"));
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
        let text = e.to_string();
        assert!(text.contains("8000000000"));
        assert!(text.contains("4000000000"));
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

    #[test]
    fn an_input_output_error_keeps_the_error_under_it() {
        use std::error::Error as _;
        let e = Error::from(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert!(e.source().is_some());
    }
}
