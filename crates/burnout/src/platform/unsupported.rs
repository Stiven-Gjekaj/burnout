//! The host that Burnout does not support.
//!
//! Burnout runs on Windows, on macOS and on Linux. On anything else it says
//! so and stops, and it does not fail to build. `burnout-core` is portable,
//! and a person on FreeBSD who builds this gets a sentence rather than a
//! compiler error in a file they did not write.
//!
//! This module compiles on every host, so a mistake in it shows up in the
//! gate rather than on the one machine that uses it.

use burnout_core::{DriveAccess, DriveId, DriveInfo, DriveList, Error, FileTarget, Result};

/// A drive layer that answers nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoAccess;

impl NoAccess {
    fn refuse<T>() -> Result<T> {
        Err(Error::Unsupported {
            target: std::env::consts::OS,
        })
    }
}

impl DriveList for NoAccess {
    fn drives(&self) -> Result<Vec<DriveInfo>> {
        Self::refuse()
    }
}

impl DriveAccess for NoAccess {
    /// A target that is never made. The trait needs a type here, and this one
    /// costs nothing because `open` below always refuses.
    type Target = FileTarget;

    fn unmount_volumes(&self, _id: &DriveId) -> Result<()> {
        Self::refuse()
    }

    fn open(&self, _id: &DriveId) -> Result<FileTarget> {
        Self::refuse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_call_names_the_host_that_is_not_supported() {
        let message = NoAccess.drives().unwrap_err().to_string();
        assert!(message.contains(std::env::consts::OS));
        assert!(NoAccess.open(&DriveId::new("disk0")).is_err());
        assert!(NoAccess.unmount_volumes(&DriveId::new("disk0")).is_err());
    }
}
