//! The Linux shim.
//!
//! This file makes system calls and decides nothing. Every decision it feeds
//! lives in [`super::parse`], whose tests run on all three hosts. Nothing
//! here is covered by a test, and nothing here may grow a rule.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use burnout_core::{BlockTarget, DriveAccess, DriveId, DriveInfo, DriveList, Error, Result};

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

/// Opens the drives of a running Linux host.
///
/// The write path takes this, and the write path is the only caller.
pub struct LinuxAccess;

impl DriveAccess for LinuxAccess {
    type Target = LinuxDisk;

    fn unmount_volumes(&self, id: &DriveId) -> Result<()> {
        let mountinfo = fs::read_to_string("/proc/self/mountinfo").map_err(|e| Error::Host {
            source: "/proc/self/mountinfo".to_string(),
            detail: e.to_string(),
        })?;
        for point in parse::mount_points_for_disk(&RealSysfs, &mountinfo, id.as_str()) {
            umount(&point)?;
        }
        Ok(())
    }

    fn open(&self, id: &DriveId) -> Result<LinuxDisk> {
        let name = id.as_str();
        let size = parse::size_bytes(&attribute_or_fail(name, "size")?)?;
        let sector_size = parse::logical_sector_size(&RealSysfs, name);

        // O_EXCL on a block device asks the kernel for the device and not
        // for a file of that name. The kernel refuses while a file system
        // holds the drive, so this cannot start a write over a mounted
        // volume. It comes after the unmount for that reason.
        let node = format!("/dev/{name}");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_EXCL)
            .open(&node)
            .map_err(|e| super::super::busy_or(e, &node))?;

        Ok(LinuxDisk {
            file,
            sector_size,
            length: size,
        })
    }
}

/// Read one attribute of a drive, or say which one is missing.
fn attribute_or_fail(name: &str, attribute: &str) -> Result<String> {
    let path = format!("/sys/block/{name}/{attribute}");
    super::source::attribute(&RealSysfs, &path).ok_or_else(|| Error::Host {
        source: path,
        detail: "the file is absent or empty".to_string(),
    })
}

/// Take one file system off its mount point.
///
/// `umount2` and not the `umount` program. A program is a tool of the host,
/// it answers in the language of the host, and forking one from a process
/// that runs as root to read its words back is not a thing to build.
fn umount(point: &str) -> Result<()> {
    let path = CString::new(point).map_err(|_| Error::Host {
        source: point.to_string(),
        detail: "the mount point holds a zero byte".to_string(),
    })?;
    // SAFETY: the pointer is a C string that lives past the call, and the
    // flags are zero, which is the plain unmount.
    let code = unsafe { libc::umount2(path.as_ptr(), 0) };
    if code != 0 {
        return Err(super::super::busy_or(io::Error::last_os_error(), point));
    }
    Ok(())
}

/// One open Linux drive.
#[derive(Debug)]
pub struct LinuxDisk {
    file: File,
    sector_size: u32,
    length: u64,
}

impl Read for LinuxDisk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for LinuxDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for LinuxDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl BlockTarget for LinuxDisk {
    fn logical_sector_size(&self) -> u32 {
        self.sector_size
    }

    fn length(&self) -> u64 {
        self.length
    }

    fn sync(&mut self) -> Result<()> {
        self.file.flush()?;
        self.file.sync_all()?;
        Ok(())
    }
}

impl Drop for LinuxDisk {
    fn drop(&mut self) {
        // Ask the kernel to read the partition table again, so that the host
        // sees the layout that was just written rather than the old one.
        // This is a courtesy and not part of the write, so a failure here
        // changes nothing and reports nothing.
        //
        // SAFETY: the descriptor is open for the length of this call.
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), BLKRRPART);
        }
    }
}

/// `BLKRRPART`, which asks the kernel to read the partition table again.
///
/// The constant is not in `libc`, and it is the same number on every
/// architecture that Linux runs on. The type is `libc::Ioctl`, because glibc
/// takes an unsigned long here and musl takes an int.
const BLKRRPART: libc::Ioctl = 0x125F;
