//! Raw mode: the image goes on the drive byte for byte.
//!
//! A Linux ISO is already a bootable disk image. It carries its own boot
//! code, its own partition table and its own EFI partition inside the file,
//! so writing it means copying the bytes and nothing else.
//!
//! Nothing here knows what a device is. It takes a source that reads and
//! seeks, a target that reads, writes, seeks and flushes, and a listener. A
//! test gives it a file and a [`crate::MemoryTarget`], which is why no test
//! in this project opens a drive.

use std::io::{Read, Seek, SeekFrom};

use crate::{
    check_aligned, check_fits, check_sector_size, round_up, BlockTarget, Digest, Error, Progress,
    ProgressEvent, Result, Sha256, Stage,
};

/// How many bytes move in one read and one write.
///
/// Four mebibytes divides by 512 and by 4096, so the same figure works on a
/// drive of either sector size. It is large enough that the cost of a system
/// call disappears, and small enough that progress moves often on a slow
/// stick.
const BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// What one write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteReport {
    /// The size of the image, in bytes.
    pub image_bytes: u64,
    /// The number of bytes that reached the drive.
    ///
    /// A drive takes whole sectors, so this is the size of the image grown to
    /// the next sector. The extra bytes are zero.
    pub written_bytes: u64,
    /// The hash of the image.
    pub digest: Digest,
}

/// What one verification proved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verification {
    /// The number of bytes that were compared.
    ///
    /// This is the size of the image and not the size of the drive. The rest
    /// of the drive is whatever was there before, and the image says nothing
    /// about it.
    pub bytes: u64,
    /// The hash that the image and the drive both gave.
    pub digest: Digest,
}

/// Copy the image onto the target, and flush before saying it is done.
pub fn write_image<S, T, P>(source: &mut S, target: &mut T, progress: &mut P) -> Result<WriteReport>
where
    S: Read + Seek,
    T: BlockTarget,
    P: Progress,
{
    let sector = check_sector_size(target.logical_sector_size())?;
    // A device holds whole sectors, so its length divides by one. A host that
    // says otherwise is a host this code has read wrongly, and the padding
    // below would then run past the end.
    check_aligned(target.length(), sector)?;

    let image_bytes = length_of(source)?;
    check_fits(image_bytes, target.length())?;

    let mut block = vec![0u8; block_bytes(sector)];
    let mut hash = Sha256::new();
    let mut done: u64 = 0;
    let mut written: u64 = 0;

    source.seek(SeekFrom::Start(0))?;
    target.seek(SeekFrom::Start(0))?;
    progress.report(ProgressEvent::Start {
        stage: Stage::Write,
        total_bytes: Some(image_bytes),
    });

    while done < image_bytes {
        let want = ((image_bytes - done) as usize).min(block.len());
        let got = fill(source, &mut block[..want])?;
        if got == 0 {
            // The file ended earlier than its own length said. Report what
            // the code can prove rather than pad the difference with zero and
            // call it a copy.
            return Err(Error::Host {
                source: "the image".to_string(),
                detail: format!("it ended after {done} bytes and it reported {image_bytes}"),
            });
        }
        hash.update(&block[..got]);

        // A drive takes whole sectors. The last read is rarely one, so the
        // tail of the block is zero and goes out with it.
        let out = round_up(got as u64, sector) as usize;
        block[got..out].fill(0);
        target.write_all(&block[..out])?;

        done += got as u64;
        written += out as u64;
        progress.report(ProgressEvent::Advance {
            stage: Stage::Write,
            bytes_done: done,
        });
    }

    // The bytes are in a cache until this returns. A counter that reached the
    // size of the image is not a finished write, so the flush comes first and
    // the report of a finished write comes after it.
    progress.report(ProgressEvent::Start {
        stage: Stage::Flush,
        total_bytes: None,
    });
    target.sync()?;
    progress.report(ProgressEvent::Done {
        stage: Stage::Flush,
        bytes_done: written,
    });
    progress.report(ProgressEvent::Done {
        stage: Stage::Write,
        bytes_done: done,
    });

    Ok(WriteReport {
        image_bytes,
        written_bytes: written,
        digest: hash.finish(),
    })
}

/// Read the drive back and compare it against the image.
///
/// This reads the first bytes of the drive, as many as the image held, and no
/// more. The drive is larger and its tail is whatever was there before, so a
/// hash of the whole drive would never match anything.
///
/// Give this a target that was opened again after the write. A read through
/// the handle that did the writing can come out of the cache of the operating
/// system, and then it proves that the cache holds the image and not that the
/// drive does.
pub fn verify_image<S, T, P>(
    source: &mut S,
    target: &mut T,
    progress: &mut P,
) -> Result<Verification>
where
    S: Read + Seek,
    T: BlockTarget,
    P: Progress,
{
    let sector = check_sector_size(target.logical_sector_size())?;
    check_aligned(target.length(), sector)?;

    let image_bytes = length_of(source)?;
    check_fits(image_bytes, target.length())?;

    let mut from_image = vec![0u8; block_bytes(sector)];
    let mut from_drive = vec![0u8; block_bytes(sector)];
    let mut image_hash = Sha256::new();
    let mut drive_hash = Sha256::new();
    let mut done: u64 = 0;
    let mut first_difference: Option<u64> = None;

    source.seek(SeekFrom::Start(0))?;
    target.seek(SeekFrom::Start(0))?;
    progress.report(ProgressEvent::Start {
        stage: Stage::Verify,
        total_bytes: Some(image_bytes),
    });

    while done < image_bytes {
        let want = ((image_bytes - done) as usize).min(from_image.len());
        let got = fill(source, &mut from_image[..want])?;
        if got == 0 {
            return Err(Error::Host {
                source: "the image".to_string(),
                detail: format!("it ended after {done} bytes and it reported {image_bytes}"),
            });
        }
        // A device answers in whole sectors, so ask for whole sectors and
        // look at the part that belongs to the image.
        let read = round_up(got as u64, sector) as usize;
        fill_exact(target, &mut from_drive[..read])?;

        if first_difference.is_none() {
            if let Some(at) = differs_at(&from_image[..got], &from_drive[..got]) {
                first_difference = Some(done + at as u64);
            }
        }
        image_hash.update(&from_image[..got]);
        drive_hash.update(&from_drive[..got]);

        done += got as u64;
        progress.report(ProgressEvent::Advance {
            stage: Stage::Verify,
            bytes_done: done,
        });
    }

    let expected = image_hash.finish();
    let got = drive_hash.finish();
    if expected != got {
        return Err(Error::VerifyFailed {
            expected: expected.to_string(),
            got: got.to_string(),
            at_byte: first_difference,
        });
    }

    progress.report(ProgressEvent::Done {
        stage: Stage::Verify,
        bytes_done: done,
    });
    Ok(Verification {
        bytes: done,
        digest: expected,
    })
}

/// A block size that is a whole number of sectors.
fn block_bytes(sector: u32) -> usize {
    let sector = sector as usize;
    (BLOCK_BYTES / sector) * sector
}

/// The length of the source, with the position put back at the start.
fn length_of<S: Read + Seek>(source: &mut S) -> Result<u64> {
    let length = source.seek(SeekFrom::End(0))?;
    source.seek(SeekFrom::Start(0))?;
    Ok(length)
}

/// Read until the buffer is full or the source ends, and say how much came.
///
/// `Read::read` may give fewer bytes than it was asked for at any time and
/// for no reason that the caller can see. A copy that treats one short read
/// as the end of the file loses the rest of the image.
fn fill<S: Read>(source: &mut S, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Read exactly as much as the buffer holds, or fail.
///
/// A short answer from a device is not an end of file. It is a device that
/// did not give what it was asked for, and carrying on would compare the
/// image against whatever the buffer held before.
fn fill_exact<T: Read>(target: &mut T, buffer: &mut [u8]) -> Result<()> {
    let filled = fill(target, buffer)?;
    if filled != buffer.len() {
        return Err(Error::Host {
            source: "the drive".to_string(),
            detail: format!(
                "it gave {filled} bytes where {} were asked for",
                buffer.len()
            ),
        });
    }
    Ok(())
}

/// The offset of the first byte that differs, if any does.
fn differs_at(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter().zip(right.iter()).position(|(a, b)| a != b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryTarget, Silent};
    use std::io::{Cursor, Write};

    /// An image whose length is not a whole number of sectors.
    ///
    /// A source that already fills whole sectors never exercises the padding,
    /// so it would pass whatever the padding did.
    fn image(bytes: usize) -> Cursor<Vec<u8>> {
        Cursor::new((0..bytes).map(|i| (i % 251) as u8).collect())
    }

    fn events(source: &mut Cursor<Vec<u8>>, target: &mut MemoryTarget) -> Vec<ProgressEvent> {
        let mut seen = Vec::new();
        {
            let mut collect = |e: ProgressEvent| seen.push(e);
            write_image(source, target, &mut collect).unwrap();
        }
        seen
    }

    #[test]
    fn every_byte_of_the_image_arrives() {
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let report = write_image(&mut source, &mut target, &mut Silent).unwrap();

        assert_eq!(report.image_bytes, 5000);
        assert_eq!(&target.contents()[..5000], source.get_ref().as_slice());
    }

    #[test]
    fn the_last_sector_is_padded_with_zero_and_the_rest_is_untouched() {
        // 5000 bytes is nine sectors and 392 bytes, so the tenth sector holds
        // 120 bytes of padding. Nothing past that sector may move.
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let report = write_image(&mut source, &mut target, &mut Silent).unwrap();

        assert_eq!(report.written_bytes, 5120);
        assert!(target.contents()[5000..5120].iter().all(|b| *b == 0));
        assert!(target.contents()[5120..].iter().all(|b| *b == 0));
    }

    #[test]
    fn the_report_carries_the_hash_of_the_image() {
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let report = write_image(&mut source, &mut target, &mut Silent).unwrap();
        assert_eq!(report.digest, crate::sha256(source.get_ref()));
    }

    #[test]
    fn the_padding_is_not_part_of_the_hash() {
        // The hash names the image. A hash that took the padding in would
        // change with the sector size of the drive, and then the number that
        // Burnout prints could not be compared with the one the publisher
        // printed.
        let mut source = image(5000);
        let mut small = MemoryTarget::new(8192, 512).unwrap();
        let mut large = MemoryTarget::new(8192, 4096).unwrap();
        let a = write_image(&mut source, &mut small, &mut Silent).unwrap();
        let b = write_image(&mut source, &mut large, &mut Silent).unwrap();

        assert_ne!(a.written_bytes, b.written_bytes);
        assert_eq!(a.digest, b.digest);
    }

    #[test]
    fn the_write_is_reported_done_only_after_the_flush_returns() {
        // A size is not a state. The bytes sit in a cache until the flush
        // comes back, so a count that reached the size of the image proves
        // nothing on its own.
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let seen = events(&mut source, &mut target);

        let flushed = seen
            .iter()
            .position(|e| {
                matches!(
                    e,
                    ProgressEvent::Done {
                        stage: Stage::Flush,
                        ..
                    }
                )
            })
            .expect("the flush is reported");
        let finished = seen
            .iter()
            .position(|e| {
                matches!(
                    e,
                    ProgressEvent::Done {
                        stage: Stage::Write,
                        ..
                    }
                )
            })
            .expect("the write is reported");
        assert!(flushed < finished);
    }

    #[test]
    fn the_flush_happens_once_and_after_the_last_byte() {
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        write_image(&mut source, &mut target, &mut Silent).unwrap();

        assert_eq!(target.sync_count(), 1);
        assert_eq!(target.bytes_at_last_sync(), 5120);
    }

    #[test]
    fn progress_counts_the_image_and_not_the_padding() {
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let seen = events(&mut source, &mut target);

        let last = seen
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Advance {
                    stage: Stage::Write,
                    bytes_done,
                } => Some(*bytes_done),
                _ => None,
            })
            .next_back()
            .expect("at least one step");
        assert_eq!(last, 5000);
    }

    #[test]
    fn an_image_larger_than_the_drive_is_refused_before_anything_is_written() {
        let mut source = image(9000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        assert!(matches!(
            write_image(&mut source, &mut target, &mut Silent),
            Err(Error::TooSmall { .. })
        ));
        assert!(target.contents().iter().all(|b| *b == 0));
    }

    #[test]
    fn an_image_that_needs_more_than_one_block_still_arrives_whole() {
        // The block is four mebibytes, so this crosses it twice and ends in
        // the middle of a sector.
        let bytes = 9 * 1024 * 1024 + 7;
        let mut source = image(bytes);
        let mut target = MemoryTarget::new(16 * 1024 * 1024, 4096).unwrap();
        let report = write_image(&mut source, &mut target, &mut Silent).unwrap();

        assert_eq!(report.image_bytes, bytes as u64);
        assert_eq!(&target.contents()[..bytes], source.get_ref().as_slice());
        assert_eq!(report.written_bytes % 4096, 0);
    }

    #[test]
    fn a_drive_that_holds_the_image_verifies() {
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let report = write_image(&mut source, &mut target, &mut Silent).unwrap();
        let proof = verify_image(&mut source, &mut target, &mut Silent).unwrap();

        assert_eq!(proof.bytes, 5000);
        assert_eq!(proof.digest, report.digest);
    }

    #[test]
    fn one_wrong_byte_fails_the_verification_and_the_message_says_where() {
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        write_image(&mut source, &mut target, &mut Silent).unwrap();

        // Reach into the target and change one byte, the way a failing stick
        // does.
        let mut broken = MemoryTarget::new(8192, 512).unwrap();
        write_image(&mut source, &mut broken, &mut Silent).unwrap();
        broken.seek(SeekFrom::Start(4096)).unwrap();
        broken.write_all(&[0xFF]).unwrap();

        match verify_image(&mut source, &mut broken, &mut Silent) {
            Err(Error::VerifyFailed { at_byte, .. }) => assert_eq!(at_byte, Some(4096)),
            other => panic!("the verification passed: {other:?}"),
        }
    }

    #[test]
    fn what_lies_past_the_image_on_the_drive_does_not_fail_the_verification() {
        // The drive is larger than the image, and its tail holds whatever was
        // there before. A check that hashed the whole drive would never pass.
        let mut source = image(5000);
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        write_image(&mut source, &mut target, &mut Silent).unwrap();

        target.seek(SeekFrom::Start(7000)).unwrap();
        target.write_all(&[0xAB; 100]).unwrap();

        assert!(verify_image(&mut source, &mut target, &mut Silent).is_ok());
    }

    #[test]
    fn a_source_that_gives_short_reads_still_arrives_whole() {
        // Read gives fewer bytes than asked for whenever it likes. A copy
        // that treats one short read as the end of the file loses the rest.
        struct Dribble(Cursor<Vec<u8>>);
        impl Read for Dribble {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let take = buf.len().min(7);
                self.0.read(&mut buf[..take])
            }
        }
        impl Seek for Dribble {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                self.0.seek(pos)
            }
        }

        let whole = image(5000).into_inner();
        let mut source = Dribble(Cursor::new(whole.clone()));
        let mut target = MemoryTarget::new(8192, 512).unwrap();
        let report = write_image(&mut source, &mut target, &mut Silent).unwrap();

        assert_eq!(report.image_bytes, 5000);
        assert_eq!(&target.contents()[..5000], whole.as_slice());
    }
}
