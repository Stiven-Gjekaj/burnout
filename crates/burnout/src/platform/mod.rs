//! The three device layers, and the choice between them.
//!
//! Each host is two files.
//!
//! `host.rs` makes system calls and decides nothing. It turns what the host
//! said into plain Rust data and hands it on. It is gated to its own host, so
//! no test reaches it.
//!
//! `parse.rs` decides everything and makes no system call. It is compiled on
//! all three hosts, so its tests run three times on every change. Everything
//! that can be wrong lives there.

use burnout_core::{DriveList, Result};

pub mod linux;
pub mod macos;
pub mod windows;

/// The drive list of the host that this build runs on.
///
/// A host that Burnout does not support gives an error and not a panic, so
/// the crate still builds and the message still says what is wrong.
pub fn drive_list() -> Result<Box<dyn DriveList>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::host::LinuxDrives))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::host::MacosDrives))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(burnout_core::Error::Unsupported {
            target: std::env::consts::OS,
        })
    }
}
