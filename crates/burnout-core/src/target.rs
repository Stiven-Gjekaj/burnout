//! The handle that Burnout writes a drive image into.
//!
//! A device on a host, and a plain file in a test. Every layer above this one
//! takes the trait and not the device, which is the only reason the tests of
//! this project touch no device.

use std::io::{Read, Seek, Write};

use crate::{DriveId, Result};

/// A thing that holds a drive image.
///
/// The write path of Burnout takes one of these, a source and a progress
/// callback, and nothing else. P2 adds that function:
///
/// ```text
/// pub fn write_image<S, T, P>(source: &mut S, target: &mut T, progress: &mut P)
///     -> Result<WriteReport>
/// where S: Read + Seek, T: BlockTarget, P: Progress;
/// ```
///
/// A test calls it with a file as the source, a
/// [`crate::MemoryTarget`] as the target and a closure as the callback.
pub trait BlockTarget: Read + Write + Seek {
    /// The smallest write that the target accepts, in bytes.
    ///
    /// A raw device refuses a write that this size does not divide.
    fn logical_sector_size(&self) -> u32;

    /// The number of bytes that the target holds.
    fn length(&self) -> u64;

    /// Put every buffered byte on the medium.
    ///
    /// The write is not complete before this call returns. A byte count that
    /// reached the size of the source proves nothing, because the operating
    /// system still holds a cache.
    ///
    /// This is a call and not a drop, because a drop reports no error and an
    /// error here loses data.
    fn sync(&mut self) -> Result<()>;
}

/// The operations that one drive needs from the host before a write.
///
/// This trait opens the drive. It does not write the image.
///
/// P1 defines it. P2 implements it on the three hosts, where the write path
/// first calls it.
pub trait DriveAccess {
    /// The handle that [`DriveAccess::open`] returns.
    type Target: BlockTarget;

    /// Take every volume of the drive away from the file system of the host.
    ///
    /// Linux calls `umount2`, macOS calls Disk Arbitration, and Windows sends
    /// `FSCTL_LOCK_VOLUME` and then `FSCTL_DISMOUNT_VOLUME`.
    ///
    /// On Windows the lock lasts only while a handle holds it, so this call
    /// does not finish the job there. The handle that `open` returns owns the
    /// volume handles, and it holds them for the whole write.
    fn unmount_volumes(&self, id: &DriveId) -> Result<()>;

    /// Open the drive for the write.
    ///
    /// The drop of the handle closes the device. Call
    /// [`BlockTarget::sync`] before the drop, because the drop reports no
    /// error.
    fn open(&self, id: &DriveId) -> Result<Self::Target>;
}
