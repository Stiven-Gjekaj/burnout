//! Byte-level access over a target that takes whole sectors only.
//!
//! A file system writes small pieces. `fatfs` writes a FAT32 entry as four
//! bytes and a directory entry as 32, each at an offset of its own choosing. A
//! raw device refuses any read or write that its sector size does not divide.
//! This sits between the two.
//!
//! A piece that covers part of a sector is read, changed and kept in a small
//! cache, and the whole sector goes back to the device later. A run of whole
//! sectors at a sector boundary skips the cache. Runs that follow one another
//! wait, and go to the device together in one write: `fatfs` writes a file one
//! cluster at a time, and one write for each cluster of 4 KiB is slow on any
//! path where each write waits for the device. A run of the size of that
//! write or more goes straight through, so a large write is not copied twice.
//!
//! Every access that this makes to the target underneath is whole sectors. A
//! test proves that by putting a [`crate::StrictTarget`] underneath, which
//! refuses anything else the way a device does.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::{BlockTarget, Result};

/// How many sectors the cache holds before it writes them back.
///
/// The metadata of a FAT32 volume clusters in a few places: the FAT, the root
/// directory and the directory of the file being written. A thousand sectors
/// holds all of those at once on either sector size.
const DEFAULT_CACHE_SECTORS: usize = 1024;

/// The most bytes that runs of whole sectors wait for before they go out.
///
/// A mebibyte is a whole number of sectors at 512 and at 4096 bytes.
const MERGE_BYTES: usize = 1024 * 1024;

/// One sector in the cache.
#[derive(Debug)]
struct Slot {
    bytes: Vec<u8>,
    dirty: bool,
}

/// Read, write and seek at any byte, over a target that takes whole sectors.
///
/// Call [`BlockTarget::sync`] or [`Write::flush`] before the drop. A drop
/// writes back what it holds as a last resort, and it reports nothing, so an
/// error there would be lost. A test build stops with a message instead.
#[derive(Debug)]
pub struct SectorIo<T: BlockTarget> {
    inner: T,
    sector: u64,
    position: u64,
    cache: BTreeMap<u64, Slot>,
    capacity: usize,
    /// Whole sectors that wait to go out in one write, and the byte of the
    /// target where they start. No sector is here and in the cache at once.
    waiting: Vec<u8>,
    waiting_at: u64,
}

impl<T: BlockTarget> SectorIo<T> {
    pub fn new(inner: T) -> Self {
        Self::with_cache(inner, DEFAULT_CACHE_SECTORS)
    }

    /// The same, with a cache of a given number of sectors.
    ///
    /// A test uses a small cache to make the write-back happen often.
    pub fn with_cache(inner: T, sectors: usize) -> Self {
        let sector = inner.logical_sector_size() as u64;
        SectorIo {
            inner,
            sector,
            position: 0,
            cache: BTreeMap::new(),
            capacity: sectors.max(1),
            waiting: Vec::new(),
            waiting_at: 0,
        }
    }

    /// Send a run of whole sectors at byte `at` of the target.
    ///
    /// A short run waits for the run that follows it. A run that does not
    /// follow the waiting bytes sends those first, so each byte reaches the
    /// target in the order it was written.
    fn send_run(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        let follows = at == self.waiting_at + self.waiting.len() as u64;
        if !self.waiting.is_empty() && !follows {
            self.send_waiting()?;
        }
        if bytes.len() >= MERGE_BYTES {
            self.send_waiting()?;
            self.inner.seek(SeekFrom::Start(at))?;
            return self.inner.write_all(bytes);
        }
        if self.waiting.is_empty() {
            self.waiting_at = at;
        }
        self.waiting.extend_from_slice(bytes);
        if self.waiting.len() >= MERGE_BYTES {
            self.send_waiting()?;
        }
        Ok(())
    }

    /// Write the waiting sectors to the target, in one write.
    fn send_waiting(&mut self) -> io::Result<()> {
        if self.waiting.is_empty() {
            return Ok(());
        }
        self.inner.seek(SeekFrom::Start(self.waiting_at))?;
        self.inner.write_all(&self.waiting)?;
        self.waiting.clear();
        Ok(())
    }

    /// Send the waiting sectors if a read of `bytes` at byte `at` of the
    /// target covers any of them, so the read gets the new bytes.
    fn send_waiting_under(&mut self, at: u64, bytes: u64) -> io::Result<()> {
        let end = self.waiting_at + self.waiting.len() as u64;
        if at < end && self.waiting_at < at + bytes {
            self.send_waiting()?;
        }
        Ok(())
    }

    /// Put one sector of the target into the cache, if it is not there.
    fn load(&mut self, index: u64) -> io::Result<()> {
        if self.cache.contains_key(&index) {
            return Ok(());
        }
        if self.cache.len() >= self.capacity {
            self.write_back()?;
            self.cache.clear();
        }
        self.send_waiting_under(index * self.sector, self.sector)?;
        let mut bytes = vec![0u8; self.sector as usize];
        self.inner.seek(SeekFrom::Start(index * self.sector))?;
        self.inner.read_exact(&mut bytes)?;
        self.cache.insert(
            index,
            Slot {
                bytes,
                dirty: false,
            },
        );
        Ok(())
    }

    /// Write every changed sector back, lowest first, and keep them cached.
    ///
    /// Sectors that follow one another go back in one write.
    fn write_back(&mut self) -> io::Result<()> {
        let dirty: Vec<u64> = self
            .cache
            .iter()
            .filter(|(_, slot)| slot.dirty)
            .map(|(index, _)| *index)
            .collect();
        let mut i = 0;
        while i < dirty.len() {
            let first = dirty[i];
            let mut run = 1;
            while i + run < dirty.len() && dirty[i + run] == first + run as u64 {
                run += 1;
            }
            let mut bytes = Vec::with_capacity(run * self.sector as usize);
            for index in first..first + run as u64 {
                bytes.extend_from_slice(&self.cache[&index].bytes);
            }
            self.inner.seek(SeekFrom::Start(first * self.sector))?;
            self.inner.write_all(&bytes)?;
            for index in first..first + run as u64 {
                if let Some(slot) = self.cache.get_mut(&index) {
                    slot.dirty = false;
                }
            }
            i += run;
        }
        Ok(())
    }

    /// How many whole sectors from `index` on are not in the cache, up to
    /// `most`.
    ///
    /// A run stops at the first cached sector, because the cache may hold a
    /// newer copy than the target does.
    fn uncached_run(&self, index: u64, most: u64) -> u64 {
        let mut run = 0;
        while run < most && !self.cache.contains_key(&(index + run)) {
            run += 1;
        }
        run
    }
}

impl<T: BlockTarget> Read for SectorIo<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want =
            (buf.len() as u64).min(self.inner.length().saturating_sub(self.position)) as usize;
        let mut done = 0;
        while done < want {
            let index = self.position / self.sector;
            let offset = self.position % self.sector;
            let left = (want - done) as u64;

            if offset == 0 && left >= self.sector {
                let run = self.uncached_run(index, left / self.sector);
                if run > 0 {
                    let bytes = (run * self.sector) as usize;
                    self.send_waiting_under(index * self.sector, bytes as u64)?;
                    self.inner.seek(SeekFrom::Start(index * self.sector))?;
                    self.inner.read_exact(&mut buf[done..done + bytes])?;
                    done += bytes;
                    self.position += bytes as u64;
                    continue;
                }
            }

            self.load(index)?;
            let take = (self.sector - offset).min(left) as usize;
            let slot = &self.cache[&index];
            buf[done..done + take]
                .copy_from_slice(&slot.bytes[offset as usize..offset as usize + take]);
            done += take;
            self.position += take as u64;
        }
        Ok(done)
    }
}

impl<T: BlockTarget> Write for SectorIo<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let room = self.inner.length().saturating_sub(self.position);
        if room == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the write runs past the end of the target",
            ));
        }
        let want = (buf.len() as u64).min(room) as usize;
        let mut done = 0;
        while done < want {
            let index = self.position / self.sector;
            let offset = self.position % self.sector;
            let left = (want - done) as u64;

            if offset == 0 && left >= self.sector {
                // Whole sectors skip the cache. A cached copy of any of them
                // is now older than what goes out, so it leaves the cache
                // rather than be written back over the new bytes.
                let run = left / self.sector;
                for i in index..index + run {
                    self.cache.remove(&i);
                }
                let bytes = (run * self.sector) as usize;
                self.send_run(index * self.sector, &buf[done..done + bytes])?;
                done += bytes;
                self.position += bytes as u64;
                continue;
            }

            self.load(index)?;
            let take = (self.sector - offset).min(left) as usize;
            let slot = self.cache.get_mut(&index).expect("the sector was loaded");
            slot.bytes[offset as usize..offset as usize + take]
                .copy_from_slice(&buf[done..done + take]);
            slot.dirty = true;
            done += take;
            self.position += take as u64;
        }
        Ok(done)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_waiting()?;
        self.write_back()?;
        self.inner.flush()
    }
}

impl<T: BlockTarget> Seek for SectorIo<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.inner.length().checked_add_signed(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
        };
        match next {
            Some(n) => {
                self.position = n;
                Ok(n)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a seek before the start of the target",
            )),
        }
    }
}

impl<T: BlockTarget> BlockTarget for SectorIo<T> {
    fn logical_sector_size(&self) -> u32 {
        self.inner.logical_sector_size()
    }

    fn length(&self) -> u64 {
        self.inner.length()
    }

    fn sync(&mut self) -> Result<()> {
        self.flush()?;
        self.inner.sync()
    }
}

impl<T: BlockTarget> Drop for SectorIo<T> {
    fn drop(&mut self) {
        if !self.waiting.is_empty() || self.cache.values().any(|slot| slot.dirty) {
            // Write back rather than lose the bytes. Nothing can report an
            // error from here, which is why the caller flushes first.
            let _ = self.send_waiting();
            let _ = self.write_back();
            debug_assert!(
                std::thread::panicking(),
                "a SectorIo was dropped with sectors that were never flushed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryTarget, StrictTarget};

    /// A small generator, so the test needs no dependency and gives the same
    /// sequence every time.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn strict(sectors: u64, sector: u32) -> StrictTarget<MemoryTarget> {
        StrictTarget::new(MemoryTarget::new(sectors * sector as u64, sector).unwrap())
    }

    /// Reads and writes at odd offsets and lengths, against a plain buffer
    /// that holds what the bytes should be.
    ///
    /// The target underneath refuses any access that is not whole sectors, so
    /// one misaligned access anywhere fails this test at once.
    fn agrees_with_a_plain_buffer(sector: u32, cache: usize, seed: u64) {
        let sectors = 64;
        let size = sectors * sector as u64;
        let mut model = vec![0u8; size as usize];
        let mut io = SectorIo::with_cache(strict(sectors, sector), cache);
        let mut rng = Rng(seed);

        for step in 0..3000 {
            let at = rng.below(size);
            let most = (size - at).min(3 * sector as u64 + 7);
            let length = (rng.below(most) + 1) as usize;
            io.seek(SeekFrom::Start(at)).unwrap();

            if rng.below(2) == 0 {
                let bytes: Vec<u8> = (0..length).map(|i| (step + i) as u8).collect();
                io.write_all(&bytes).unwrap();
                model[at as usize..at as usize + length].copy_from_slice(&bytes);
            } else {
                let mut bytes = vec![0u8; length];
                io.read_exact(&mut bytes).unwrap();
                assert_eq!(
                    bytes,
                    &model[at as usize..at as usize + length],
                    "a read at {at} of {length} bytes, step {step}"
                );
            }
        }

        io.sync().unwrap();
        let written = io.inner.inner().contents().to_vec();
        drop(io);
        assert_eq!(written, model, "the target holds what was written");
    }

    #[test]
    fn odd_reads_and_writes_agree_with_a_plain_buffer_at_512() {
        agrees_with_a_plain_buffer(512, DEFAULT_CACHE_SECTORS, 0x5EED_0001);
    }

    #[test]
    fn odd_reads_and_writes_agree_with_a_plain_buffer_at_4096() {
        agrees_with_a_plain_buffer(4096, DEFAULT_CACHE_SECTORS, 0x5EED_0002);
    }

    #[test]
    fn a_cache_that_fills_often_still_agrees_with_a_plain_buffer() {
        // Four sectors of cache over sixty-four of drive makes the write-back
        // happen all the time, which is the path a large copy takes.
        agrees_with_a_plain_buffer(512, 4, 0x5EED_0003);
        agrees_with_a_plain_buffer(4096, 4, 0x5EED_0004);
    }

    #[test]
    fn a_small_write_reaches_the_target_only_at_the_flush() {
        let mut io = SectorIo::new(strict(8, 512));
        io.seek(SeekFrom::Start(100)).unwrap();
        io.write_all(&[0xAB; 4]).unwrap();
        assert!(io.inner.inner().contents().iter().all(|b| *b == 0));

        io.flush().unwrap();
        assert_eq!(&io.inner.inner().contents()[100..104], &[0xAB; 4]);
    }

    #[test]
    fn a_read_sees_a_small_write_before_the_flush() {
        let mut io = SectorIo::new(strict(8, 512));
        io.seek(SeekFrom::Start(510)).unwrap();
        io.write_all(&[1, 2, 3, 4]).unwrap();
        io.seek(SeekFrom::Start(510)).unwrap();
        let mut back = [0u8; 4];
        io.read_exact(&mut back).unwrap();
        assert_eq!(back, [1, 2, 3, 4]);
        io.flush().unwrap();
    }

    #[test]
    fn whole_sectors_skip_the_cache_and_reach_the_target_at_the_flush() {
        let mut io = SectorIo::new(strict(8, 512));
        io.write_all(&[7; 4 * 512]).unwrap();
        assert!(io.cache.is_empty());
        io.flush().unwrap();
        assert_eq!(&io.inner.inner().contents()[..2048], &[7; 2048][..]);
    }

    /// A target that counts the writes that reach it.
    struct Counted {
        inner: MemoryTarget,
        writes: usize,
    }

    impl Read for Counted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Write for Counted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for Counted {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    impl BlockTarget for Counted {
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

    fn counted(bytes: u64) -> SectorIo<Counted> {
        SectorIo::new(Counted {
            inner: MemoryTarget::new(bytes, 512).unwrap(),
            writes: 0,
        })
    }

    #[test]
    fn clusters_that_follow_one_another_reach_the_target_as_few_writes() {
        // Three hundred clusters of 4 KiB, one after another, the way fatfs
        // writes a file. They go out as one mebibyte and then the rest.
        let mut io = counted(2 * MERGE_BYTES as u64);
        for cluster in 0..300u32 {
            io.write_all(&[cluster as u8; 4096]).unwrap();
        }
        io.flush().unwrap();
        assert_eq!(io.inner.writes, 2);
        for cluster in 0..300usize {
            let at = cluster * 4096;
            assert!(io.inner.inner.contents()[at..at + 4096]
                .iter()
                .all(|b| *b == cluster as u8));
        }
    }

    #[test]
    fn a_gap_sends_the_waiting_sectors_first() {
        let mut io = counted(64 * 512);
        io.write_all(&[1; 1024]).unwrap();
        io.seek(SeekFrom::Start(8 * 512)).unwrap();
        io.write_all(&[2; 1024]).unwrap();
        io.flush().unwrap();
        assert_eq!(io.inner.writes, 2);
        let contents = io.inner.inner.contents();
        assert!(contents[..1024].iter().all(|b| *b == 1));
        assert!(contents[1024..8 * 512].iter().all(|b| *b == 0));
        assert!(contents[8 * 512..9 * 512 + 512].iter().all(|b| *b == 2));
    }

    #[test]
    fn a_write_of_a_mebibyte_or_more_goes_straight_through() {
        let mut io = counted(4 * MERGE_BYTES as u64);
        io.write_all(&vec![3; 2 * MERGE_BYTES]).unwrap();
        assert_eq!(io.inner.writes, 1);
        assert!(io.waiting.is_empty());
        io.flush().unwrap();
        assert_eq!(io.inner.writes, 1);
    }

    #[test]
    fn a_read_of_waiting_sectors_gets_the_new_bytes() {
        let mut io = SectorIo::new(strict(8, 512));
        io.write_all(&[5; 2 * 512]).unwrap();
        io.seek(SeekFrom::Start(0)).unwrap();
        let mut whole = [0u8; 2 * 512];
        io.read_exact(&mut whole).unwrap();
        assert_eq!(whole, [5; 2 * 512]);

        // And a small write into a waiting sector reads the sector back
        // first, so it must find the new bytes there too.
        io.write_all(&[6; 512]).unwrap();
        io.seek(SeekFrom::Start(2 * 512 + 10)).unwrap();
        io.write_all(&[9; 4]).unwrap();
        io.flush().unwrap();
        let contents = io.inner.inner().contents();
        assert!(contents[1024..1034].iter().all(|b| *b == 6));
        assert_eq!(&contents[1034..1038], &[9; 4]);
        assert!(contents[1038..1536].iter().all(|b| *b == 6));
    }

    #[test]
    fn a_whole_sector_written_over_a_cached_one_wins_at_the_flush() {
        // The cache holds the sector with four new bytes in it. A whole
        // sector then goes straight to the target. The flush must not write
        // the older cached copy back over it.
        let mut io = SectorIo::new(strict(8, 512));
        io.write_all(&[1; 4]).unwrap();
        io.seek(SeekFrom::Start(0)).unwrap();
        io.write_all(&[2; 512]).unwrap();
        io.flush().unwrap();
        assert_eq!(&io.inner.inner().contents()[..512], &[2; 512][..]);
    }

    #[test]
    fn a_read_stops_at_the_end_and_a_write_past_it_is_refused() {
        let mut io = SectorIo::new(strict(2, 512));
        io.seek(SeekFrom::Start(1000)).unwrap();
        let mut buf = [0u8; 100];
        assert_eq!(io.read(&mut buf).unwrap(), 24);
        assert_eq!(io.read(&mut buf).unwrap(), 0);
        assert!(io.write(&[1]).is_err());
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "never flushed")]
    fn a_drop_with_unwritten_sectors_stops_a_test_build() {
        // A caller that forgets the flush loses the only place an error can
        // be reported. The test build makes that a failure and not a silence.
        let mut io = SectorIo::new(strict(8, 512));
        io.write_all(&[1; 4]).unwrap();
        drop(io);
    }
}
