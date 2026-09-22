//! A drive image in memory, which counts what the caller asked of it.
//!
//! A test uses this to read the bytes back without a file, and to prove that
//! the code above it flushes before it reports that a write is complete.

use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use crate::{check_aligned, check_sector_size, BlockTarget, Result};

/// A drive image held in memory.
#[derive(Debug)]
pub struct MemoryTarget {
    bytes: Cursor<Vec<u8>>,
    sector_size: u32,
    length: u64,
    sync_count: u32,
    written: u64,
    bytes_at_last_sync: u64,
}

impl MemoryTarget {
    /// Make an image of the given length, filled with zero.
    pub fn new(length: u64, sector_size: u32) -> Result<Self> {
        let sector_size = check_sector_size(sector_size)?;
        check_aligned(length, sector_size)?;
        Ok(MemoryTarget {
            bytes: Cursor::new(vec![0u8; length as usize]),
            sector_size,
            length,
            sync_count: 0,
            written: 0,
            bytes_at_last_sync: 0,
        })
    }

    /// Every byte of the image.
    pub fn contents(&self) -> &[u8] {
        self.bytes.get_ref()
    }

    /// How many times the caller asked for a flush.
    ///
    /// A test reads this to prove that the code reported a write complete
    /// after the flush returned, and not when a counter reached the size of
    /// the source. A size is not a state.
    pub fn sync_count(&self) -> u32 {
        self.sync_count
    }

    /// How many bytes the caller had written at the last flush.
    ///
    /// A test compares this against the size of the source. If it is smaller,
    /// the code reported success while bytes were still in a cache.
    pub fn bytes_at_last_sync(&self) -> u64 {
        self.bytes_at_last_sync
    }

    /// How many bytes the caller has written so far.
    ///
    /// This counts what each write took, and not the position. A caller that
    /// seeks back and writes the start last ends at the start, and that
    /// position says nothing about how much it wrote.
    pub fn bytes_written(&self) -> u64 {
        self.written
    }
}

impl Read for MemoryTarget {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.bytes.read(buf)
    }
}

impl Write for MemoryTarget {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // A drive does not grow. Refuse a write that runs past the end, the
        // way a device refuses one.
        let room = self.length.saturating_sub(self.bytes.position());
        if room == 0 && !buf.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "the write runs past the end of the target",
            ));
        }
        let take = buf.len().min(room as usize);
        let took = self.bytes.write(&buf[..take])?;
        self.written += took as u64;
        Ok(took)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.bytes.flush()
    }
}

impl Seek for MemoryTarget {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.bytes.seek(pos)
    }
}

impl BlockTarget for MemoryTarget {
    fn logical_sector_size(&self) -> u32 {
        self.sector_size
    }

    fn length(&self) -> u64 {
        self.length
    }

    fn sync(&mut self) -> Result<()> {
        self.bytes.flush()?;
        self.sync_count += 1;
        self.bytes_at_last_sync = self.written;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_target_is_full_of_zero() {
        let t = MemoryTarget::new(1024, 512).unwrap();
        assert_eq!(t.contents().len(), 1024);
        assert!(t.contents().iter().all(|b| *b == 0));
    }

    #[test]
    fn the_bytes_come_back_the_way_they_went_in() {
        let mut t = MemoryTarget::new(1024, 512).unwrap();
        t.write_all(&[0x5A; 512]).unwrap();
        assert_eq!(&t.contents()[..512], &[0x5A; 512]);
        assert!(t.contents()[512..].iter().all(|b| *b == 0));
    }

    #[test]
    fn a_target_counts_no_flush_before_one_is_asked_for() {
        let mut t = MemoryTarget::new(512, 512).unwrap();
        t.write_all(&[1u8; 512]).unwrap();
        assert_eq!(t.sync_count(), 0);
        assert_eq!(t.bytes_at_last_sync(), 0);
    }

    #[test]
    fn a_flush_records_how_far_the_write_had_reached() {
        // This is what makes "a size is not a state" a rule that a test can
        // check, rather than a sentence in a document.
        let mut t = MemoryTarget::new(1024, 512).unwrap();
        t.write_all(&[1u8; 512]).unwrap();
        t.sync().unwrap();
        assert_eq!(t.sync_count(), 1);
        assert_eq!(t.bytes_at_last_sync(), 512);

        t.write_all(&[2u8; 512]).unwrap();
        assert_eq!(
            t.bytes_at_last_sync(),
            512,
            "the second half is not on the medium yet"
        );
        t.sync().unwrap();
        assert_eq!(t.sync_count(), 2);
        assert_eq!(t.bytes_at_last_sync(), 1024);
    }

    #[test]
    fn a_write_that_goes_back_to_the_start_still_counts() {
        let mut t = MemoryTarget::new(1024, 512).unwrap();
        t.seek(SeekFrom::Start(512)).unwrap();
        t.write_all(&[1u8; 512]).unwrap();
        t.seek(SeekFrom::Start(0)).unwrap();
        t.write_all(&[2u8; 512]).unwrap();
        t.sync().unwrap();
        assert_eq!(t.bytes_written(), 1024);
        assert_eq!(t.bytes_at_last_sync(), 1024);
    }

    #[test]
    fn a_write_past_the_end_stops_at_the_end() {
        let mut t = MemoryTarget::new(512, 512).unwrap();
        let wrote = t.write(&[1u8; 1024]).unwrap();
        assert_eq!(wrote, 512);
        assert!(t.write(&[1u8; 1]).is_err());
    }

    #[test]
    fn a_ragged_length_is_refused() {
        assert!(MemoryTarget::new(1000, 512).is_err());
        assert!(MemoryTarget::new(512, 4096).is_err());
    }

    #[test]
    fn a_target_of_4096_byte_sectors_works_the_same_way() {
        let mut t = MemoryTarget::new(8192, 4096).unwrap();
        assert_eq!(t.logical_sector_size(), 4096);
        t.write_all(&[3u8; 4096]).unwrap();
        t.sync().unwrap();
        assert_eq!(t.bytes_at_last_sync(), 4096);
    }
}
