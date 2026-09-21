//! A target that refuses what a raw device refuses.
//!
//! A file takes a write of any length at any offset. A raw device does not:
//! macOS answers a misaligned write to `/dev/rdiskN` with an error, and so do
//! Linux with `O_DIRECT` and Windows on a physical drive. A test that writes a
//! file therefore proves nothing about alignment, and the code above passes it
//! whatever it does.
//!
//! This wrapper puts the rule of the device in front of a file or of memory,
//! so a test fails the moment anything above it issues one unaligned access.

use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::{BlockTarget, Result};

/// A target that takes whole sectors only.
#[derive(Debug)]
pub struct StrictTarget<T: BlockTarget> {
    inner: T,
    position: u64,
}

impl<T: BlockTarget> StrictTarget<T> {
    pub fn new(inner: T) -> Self {
        StrictTarget { inner, position: 0 }
    }

    /// The target underneath, to read its bytes back.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Give the target underneath back.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Refuse an access that a raw device would refuse.
    fn check(&self, length: usize) -> io::Result<()> {
        let sector = self.inner.logical_sector_size() as u64;
        if self.position % sector != 0 || length as u64 % sector != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "an access of {length} bytes at {} is not whole {sector} byte sectors",
                    self.position
                ),
            ));
        }
        Ok(())
    }
}

impl<T: BlockTarget> Read for StrictTarget<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check(buf.len())?;
        let n = self.inner.read(buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

impl<T: BlockTarget> Write for StrictTarget<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.check(buf.len())?;
        let n = self.inner.write(buf)?;
        self.position += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T: BlockTarget> Seek for StrictTarget<T> {
    /// A seek goes anywhere, as it does on a device. The refusal comes with
    /// the access that follows it.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = self.inner.seek(pos)?;
        Ok(self.position)
    }
}

impl<T: BlockTarget> BlockTarget for StrictTarget<T> {
    fn logical_sector_size(&self) -> u32 {
        self.inner.logical_sector_size()
    }

    fn length(&self) -> u64 {
        self.inner.length()
    }

    fn sync(&mut self) -> Result<()> {
        self.inner.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryTarget;

    fn strict(sector: u32) -> StrictTarget<MemoryTarget> {
        StrictTarget::new(MemoryTarget::new(8 * 4096, sector).unwrap())
    }

    #[test]
    fn whole_sectors_at_a_sector_boundary_go_through() {
        let mut t = strict(512);
        t.seek(SeekFrom::Start(1024)).unwrap();
        t.write_all(&[9; 1024]).unwrap();
        t.seek(SeekFrom::Start(1024)).unwrap();
        let mut back = [0u8; 512];
        t.read_exact(&mut back).unwrap();
        assert_eq!(back, [9; 512]);
    }

    #[test]
    fn a_write_that_starts_inside_a_sector_is_refused() {
        let mut t = strict(512);
        t.seek(SeekFrom::Start(4)).unwrap();
        let e = t.write(&[1; 512]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_write_of_part_of_a_sector_is_refused() {
        // A FAT32 entry is four bytes. This is the write that fatfs makes,
        // and the one that a raw device refuses.
        let mut t = strict(512);
        assert!(t.write(&[1; 4]).is_err());
    }

    #[test]
    fn a_read_of_part_of_a_sector_is_refused() {
        let mut t = strict(512);
        let mut small = [0u8; 32];
        assert!(t.read(&mut small).is_err());
    }

    #[test]
    fn the_rule_follows_the_sector_size_of_the_target_underneath() {
        // 512 bytes is a whole sector on one drive and part of one on a 4Kn
        // drive.
        let mut t = strict(4096);
        assert!(t.write(&[1; 512]).is_err());
        assert!(t.write(&[1; 4096]).is_ok());
    }

    #[test]
    fn a_refused_write_changes_nothing() {
        let mut t = strict(512);
        let _ = t.write(&[0xFF; 4]);
        assert!(t.inner().contents().iter().all(|b| *b == 0));
    }
}
