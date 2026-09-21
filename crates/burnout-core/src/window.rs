//! One partition of a drive, as a target of its own.
//!
//! A file system writes from its own byte 0. On a drive it lives at an offset,
//! and it must stop where its partition stops. This gives it both: position 0
//! of the window is the first byte of the partition, and the window refuses a
//! write past its end, so a FAT32 volume cannot run into the partition after
//! it.

use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::{check_aligned, BlockTarget, Error, Result};

/// A range of whole sectors inside a target.
#[derive(Debug)]
pub struct Window<T: BlockTarget> {
    inner: T,
    start: u64,
    length: u64,
    position: u64,
}

impl<T: BlockTarget> Window<T> {
    /// A window of `length` bytes that starts at byte `start` of `inner`.
    ///
    /// Both have to be whole sectors, because a partition is, and the window
    /// has to fit inside the target.
    pub fn new(inner: T, start: u64, length: u64) -> Result<Self> {
        let sector = inner.logical_sector_size();
        check_aligned(start, sector)?;
        check_aligned(length, sector)?;
        let end = start.checked_add(length).ok_or(Error::TooSmall {
            image_bytes: u64::MAX,
            drive_bytes: inner.length(),
        })?;
        if end > inner.length() {
            return Err(Error::TooSmall {
                image_bytes: end,
                drive_bytes: inner.length(),
            });
        }
        Ok(Window {
            inner,
            start,
            length,
            position: 0,
        })
    }

    /// The first byte of the window, counted from the start of the target.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Give the target underneath back.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// How many bytes of the window lie at and after the position.
    fn remaining(&self) -> u64 {
        self.length.saturating_sub(self.position)
    }

    /// Put the target underneath at the byte that the position names.
    ///
    /// This runs before every access and not only after a seek, so nothing
    /// else that moved the target can make the window write in the wrong
    /// place.
    fn place(&mut self) -> io::Result<()> {
        self.inner
            .seek(SeekFrom::Start(self.start + self.position))
            .map(|_| ())
    }
}

impl<T: BlockTarget> Read for Window<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let take = (buf.len() as u64).min(self.remaining()) as usize;
        if take == 0 {
            return Ok(0);
        }
        self.place()?;
        let n = self.inner.read(&mut buf[..take])?;
        self.position += n as u64;
        Ok(n)
    }
}

impl<T: BlockTarget> Write for Window<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let take = (buf.len() as u64).min(self.remaining()) as usize;
        if take == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the write runs past the end of the partition",
            ));
        }
        self.place()?;
        let n = self.inner.write(&buf[..take])?;
        self.position += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T: BlockTarget> Seek for Window<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.length.checked_add_signed(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
        };
        match next {
            Some(n) => {
                self.position = n;
                Ok(n)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a seek before the start of the partition",
            )),
        }
    }
}

impl<T: BlockTarget> BlockTarget for Window<T> {
    fn logical_sector_size(&self) -> u32 {
        self.inner.logical_sector_size()
    }

    fn length(&self) -> u64 {
        self.length
    }

    fn sync(&mut self) -> Result<()> {
        self.inner.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryTarget, StrictTarget};

    #[test]
    fn byte_zero_of_the_window_is_the_first_byte_of_the_partition() {
        let mut drive = MemoryTarget::new(8 * 512, 512).unwrap();
        {
            let mut w = Window::new(&mut drive, 1024, 2048).unwrap();
            w.write_all(&[5; 512]).unwrap();
        }
        assert!(drive.contents()[..1024].iter().all(|b| *b == 0));
        assert!(drive.contents()[1024..1536].iter().all(|b| *b == 5));
    }

    #[test]
    fn a_write_past_the_end_is_refused_and_the_next_partition_is_untouched() {
        // A file system that believed it was larger than its partition would
        // otherwise write into the partition after it.
        let mut drive = MemoryTarget::new(8 * 512, 512).unwrap();
        {
            let mut w = Window::new(&mut drive, 1024, 1024).unwrap();
            w.seek(SeekFrom::Start(512)).unwrap();
            assert!(w.write_all(&[7; 1024]).is_err());
        }
        assert!(drive.contents()[2048..].iter().all(|b| *b == 0));
    }

    #[test]
    fn a_read_stops_at_the_end_of_the_window() {
        let mut drive = MemoryTarget::new(8 * 512, 512).unwrap();
        let mut w = Window::new(&mut drive, 512, 1024).unwrap();
        w.seek(SeekFrom::Start(512)).unwrap();
        let mut buf = [0u8; 2048];
        let mut total = 0;
        loop {
            match w.read(&mut buf[total..]).unwrap() {
                0 => break,
                n => total += n,
            }
        }
        assert_eq!(total, 512);
    }

    #[test]
    fn a_window_has_to_be_whole_sectors_and_fit_inside_the_drive() {
        let mut drive = MemoryTarget::new(8 * 512, 512).unwrap();
        assert!(Window::new(&mut drive, 100, 512).is_err());
        assert!(Window::new(&mut drive, 512, 100).is_err());
        assert!(Window::new(&mut drive, 512, 8 * 512).is_err());
        assert!(Window::new(&mut drive, 0, 8 * 512).is_ok());
    }

    #[test]
    fn a_window_keeps_the_alignment_of_what_goes_through_it() {
        // The window starts on a sector, so an aligned access stays aligned
        // on the drive and a raw device takes it.
        let mut drive = StrictTarget::new(MemoryTarget::new(16 * 4096, 4096).unwrap());
        let mut w = Window::new(&mut drive, 4096, 8 * 4096).unwrap();
        w.seek(SeekFrom::Start(4096)).unwrap();
        w.write_all(&[3; 4096]).unwrap();
        assert!(w.write(&[3; 100]).is_err());
    }

    #[test]
    fn the_end_of_the_window_is_its_own_end_and_not_the_drive_end() {
        let mut drive = MemoryTarget::new(8 * 512, 512).unwrap();
        let mut w = Window::new(&mut drive, 512, 2048).unwrap();
        assert_eq!(w.seek(SeekFrom::End(0)).unwrap(), 2048);
        assert_eq!(w.length(), 2048);
        assert!(w.seek(SeekFrom::Current(-4096)).is_err());
    }
}
