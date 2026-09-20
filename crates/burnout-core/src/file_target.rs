//! A drive image in a plain file.
//!
//! This is what a test writes into. It is also what a person gets when they
//! ask Burnout to build an image rather than a drive.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{check_aligned, check_sector_size, BlockTarget, Result};

/// A file that stands in for a drive.
#[derive(Debug)]
pub struct FileTarget {
    file: File,
    sector_size: u32,
    length: u64,
}

impl FileTarget {
    /// Wrap a file that is already open.
    ///
    /// The sector size must be 512 or 4096, and it must divide the length of
    /// the file, because a drive of that sector size holds whole sectors
    /// only.
    pub fn new(file: File, sector_size: u32) -> Result<Self> {
        let sector_size = check_sector_size(sector_size)?;
        let length = file.metadata()?.len();
        check_aligned(length, sector_size)?;
        Ok(FileTarget {
            file,
            sector_size,
            length,
        })
    }

    /// Make a file of the given length, and wrap it.
    pub fn create(path: &Path, length: u64, sector_size: u32) -> Result<Self> {
        let sector_size = check_sector_size(sector_size)?;
        check_aligned(length, sector_size)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(length)?;
        Ok(FileTarget {
            file,
            sector_size,
            length,
        })
    }
}

impl Read for FileTarget {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for FileTarget {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for FileTarget {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }
}

impl BlockTarget for FileTarget {
    fn logical_sector_size(&self) -> u32 {
        self.sector_size
    }

    fn length(&self) -> u64 {
        self.length
    }

    fn sync(&mut self) -> Result<()> {
        // Two steps. The first empties the buffer of this process, and the
        // second tells the operating system to put the bytes on the medium.
        self.file.flush()?;
        self.file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A file in the temporary directory that goes away with the test.
    ///
    /// This is a few lines rather than a dependency, so that this crate keeps
    /// no dependency at all, not even one for the tests.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNT: AtomicU32 = AtomicU32::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            let name = format!("burnout-{}-{}-{}.img", std::process::id(), tag, n);
            Scratch(std::env::temp_dir().join(name))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_new_file_holds_the_length_that_was_asked_for() {
        let s = Scratch::new("length");
        let t = FileTarget::create(s.path(), 4096, 512).unwrap();
        assert_eq!(t.length(), 4096);
        assert_eq!(t.logical_sector_size(), 512);
    }

    #[test]
    fn the_bytes_come_back_the_way_they_went_in() {
        let s = Scratch::new("round");
        let mut t = FileTarget::create(s.path(), 1024, 512).unwrap();
        t.write_all(&[0xA5; 512]).unwrap();
        t.sync().unwrap();
        t.seek(SeekFrom::Start(0)).unwrap();
        let mut back = [0u8; 512];
        t.read_exact(&mut back).unwrap();
        assert_eq!(back, [0xA5; 512]);
    }

    #[test]
    fn a_write_lands_where_the_seek_put_it() {
        let s = Scratch::new("seek");
        let mut t = FileTarget::create(s.path(), 2048, 512).unwrap();
        t.seek(SeekFrom::Start(1024)).unwrap();
        t.write_all(&[7u8; 4]).unwrap();
        t.sync().unwrap();
        t.seek(SeekFrom::Start(1020)).unwrap();
        let mut back = [0u8; 8];
        t.read_exact(&mut back).unwrap();
        assert_eq!(back, [0, 0, 0, 0, 7, 7, 7, 7]);
    }

    #[test]
    fn a_sector_size_that_no_drive_reports_is_refused() {
        let s = Scratch::new("sector");
        assert!(FileTarget::create(s.path(), 4096, 1024).is_err());
    }

    #[test]
    fn a_length_that_does_not_fill_whole_sectors_is_refused() {
        let s = Scratch::new("ragged");
        assert!(FileTarget::create(s.path(), 1000, 512).is_err());
    }

    #[test]
    fn a_file_of_4096_byte_sectors_works_the_same_way() {
        // The drive that nobody has on their desk, and that every image
        // eventually meets.
        let s = Scratch::new("4kn");
        let mut t = FileTarget::create(s.path(), 8192, 4096).unwrap();
        assert_eq!(t.logical_sector_size(), 4096);
        t.write_all(&[1u8; 4096]).unwrap();
        t.sync().unwrap();
        assert_eq!(t.length(), 8192);
    }

    #[test]
    fn an_existing_file_of_the_wrong_length_is_refused() {
        let s = Scratch::new("existing");
        {
            let f = File::create(s.path()).unwrap();
            f.set_len(1000).unwrap();
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(s.path())
            .unwrap();
        assert!(FileTarget::new(f, 512).is_err());
    }
}
