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
pub mod unsupported;
pub mod windows;

/// The type that opens a drive on the host that this build runs on.
///
/// This is a type and not a trait object. [`burnout_core::DriveAccess`] names
/// the handle that `open` gives back, and that handle is a different type on
/// each host, so a boxed trait would have to name it. One host is compiled at
/// a time, so the concrete type is known and nothing needs boxing.
#[cfg(target_os = "linux")]
pub type HostAccess = linux::host::LinuxAccess;
#[cfg(target_os = "macos")]
pub type HostAccess = macos::host::MacosAccess;
#[cfg(windows)]
pub type HostAccess = windows::host::WindowsAccess;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub type HostAccess = unsupported::NoAccess;

/// The way to open a drive on the host that this build runs on.
///
/// A host that Burnout does not support gives an error and not a panic.
pub fn drive_access() -> Result<HostAccess> {
    #[cfg(target_os = "linux")]
    {
        Ok(linux::host::LinuxAccess)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(macos::host::MacosAccess)
    }
    #[cfg(windows)]
    {
        Ok(windows::host::WindowsAccess)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(burnout_core::Error::Unsupported {
            target: std::env::consts::OS,
        })
    }
}

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
    #[cfg(windows)]
    {
        Ok(Box::new(windows::host::WindowsDrives))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(burnout_core::Error::Unsupported {
            target: std::env::consts::OS,
        })
    }
}
