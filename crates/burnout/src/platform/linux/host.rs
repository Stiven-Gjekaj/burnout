//! The Linux shim.
//!
//! This file makes system calls and decides nothing. Every decision it feeds
//! lives in [`super::parse`], whose tests run on all three hosts. Nothing
//! here is covered by a test, and nothing here may grow a rule.

use std::fs;
use std::io;

use burnout_core::{DriveInfo, DriveList, Error, Result};

use super::parse;
use super::source::SysfsSource;

/// Reads the `sysfs` of the running kernel.
pub struct RealSysfs;

impl SysfsSource for RealSysfs {
    fn read_file(&self, path: &str) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn read_link(&self, path: &str) -> io::Result<String> {
        Ok(fs::read_link(path)?.to_string_lossy().into_owned())
    }
}

/// Lists the drives of a running Linux host.
pub struct LinuxDrives;

impl DriveList for LinuxDrives {
    fn drives(&self) -> Result<Vec<DriveInfo>> {
        // An absent file is an empty one here. A machine with no swap has no
        // /proc/swaps to read, and that is not a failure.
        let mountinfo = fs::read_to_string("/proc/self/mountinfo").map_err(|e| Error::Host {
            source: "/proc/self/mountinfo".to_string(),
            detail: e.to_string(),
        })?;
        let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
        parse::list_drives(&RealSysfs, &mountinfo, &swaps)
    }
}
