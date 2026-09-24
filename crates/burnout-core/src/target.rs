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

    /// Keep the host from mounting a volume of the drive for as long as the
    /// returned value lives.
    ///
    /// macOS mounts the volumes of a new table when the handle that wrote it
    /// closes, and Spotlight then writes to them before the check reads them.
    /// A host that does not do this keeps the default, which holds nothing.
    fn keep_unmounted(&self, _id: &DriveId) -> Result<Box<dyn std::any::Any>> {
        Ok(Box::new(()))
    }

    /// Take the drive away from the host when the write and its check end.
    ///
    /// macOS mounts a drive as soon as nothing holds it, and Spotlight then
    /// writes its index onto the drive. An eject stops that until the drive
    /// is connected again. A host that does not do this keeps the default,
    /// which does nothing. Windows already holds the drive offline.
    fn eject(&self, _id: &DriveId) -> Result<()> {
        Ok(())
    }

    /// Open the drive for the write.
    ///
    /// The drop of the handle closes the device. Call
    /// [`BlockTarget::sync`] before the drop, because the drop reports no
    /// error.
    fn open(&self, id: &DriveId) -> Result<Self::Target>;
}

/// A borrowed target is a target.
///
/// The layout of a Windows drive puts one file system inside a window onto
/// the drive, and then checks it through a second mount. Both need the drive
/// for a while and then give it back, and a borrow is how a caller keeps it.
impl<T: BlockTarget + ?Sized> BlockTarget for &mut T {
    fn logical_sector_size(&self) -> u32 {
        (**self).logical_sector_size()
    }

    fn length(&self) -> u64 {
        (**self).length()
    }

    fn sync(&mut self) -> Result<()> {
        (**self).sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryTarget;
    use std::io::{Read, Seek, SeekFrom};

    fn takes_a_target<T: BlockTarget>(mut target: T) -> (u32, u64) {
        target.seek(SeekFrom::Start(512)).unwrap();
        target.write_all(&[7; 512]).unwrap();
        target.sync().unwrap();
        (target.logical_sector_size(), target.length())
    }

    #[test]
    fn a_borrowed_target_writes_into_the_target_it_borrows() {
        let mut owned = MemoryTarget::new(4096, 512).unwrap();
        assert_eq!(takes_a_target(&mut owned), (512, 4096));

        // The borrow ended and the owner still holds what went through it.
        assert_eq!(owned.sync_count(), 1);
        let mut back = [0u8; 512];
        owned.seek(SeekFrom::Start(512)).unwrap();
        owned.read_exact(&mut back).unwrap();
        assert_eq!(back, [7; 512]);
    }
}
