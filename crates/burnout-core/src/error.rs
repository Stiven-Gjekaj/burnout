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
    fn an_input_output_error_keeps_the_error_under_it() {
        use std::error::Error as _;
        let e = Error::from(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert!(e.source().is_some());
    }
}
