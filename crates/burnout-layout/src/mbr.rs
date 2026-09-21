//! The partition table, and the removal of whatever table was there before.
//!
//! Burnout writes the table itself, so the spare EFI partition that
//! `diskutil` adds on macOS never appears, and the table is the same on
//! three hosts. [The milestones] record why it is MBR and not GPT.
//!
//! [The milestones]: https://github.com/Stiven-Gjekaj/burnout/blob/main/docs/milestones.md

use std::io::SeekFrom;

use burnout_core::{BlockTarget, Result};

use crate::{Extent, Layout, ALIGN};

/// FAT32 with LBA addressing.
pub const TYPE_FAT32: u8 = 0x0C;

/// exFAT. It shares the byte with NTFS, and the file system says which.
pub const TYPE_EXFAT: u8 = 0x07;

/// The byte that marks the partition a BIOS starts from.
const ACTIVE: u8 = 0x80;

/// The 512 bytes of an MBR.
///
/// The boot code is zero. UEFI firmware never runs it, and Legacy BIOS boot
/// is a door this leaves open for later.
///
/// `disk_signature` is the number Windows uses to tell drives apart. Two
/// drives with one number make Windows take the second offline, so the
/// caller gives a number of its own.
pub fn mbr_bytes(layout: &Layout, disk_signature: u32) -> [u8; 512] {
    let mut mbr = [0u8; 512];
    mbr[440..444].copy_from_slice(&disk_signature.to_le_bytes());
    mbr[446..462].copy_from_slice(&entry(layout, layout.boot, TYPE_FAT32, true));
    mbr[462..478].copy_from_slice(&entry(layout, layout.install, TYPE_EXFAT, false));
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    mbr
}

/// One entry of the table.
fn entry(layout: &Layout, extent: Extent, kind: u8, active: bool) -> [u8; 16] {
    let first = layout.first_sector(extent);
    let count = layout.sectors(extent);
    let mut e = [0u8; 16];
    e[0] = if active { ACTIVE } else { 0 };
    e[1..4].copy_from_slice(&chs(first));
    e[4] = kind;
    e[5..8].copy_from_slice(&chs(first + count - 1));
    // The plan keeps every partition inside what 32 bits can count.
    e[8..12].copy_from_slice(&(first as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(count as u32).to_le_bytes());
    e
}

/// A cylinder, head and sector address, in the form that every tool writes.
///
/// No drive of the last thirty years has had a geometry. Every tool writes
/// these fields from a made-up one of 255 heads and 63 sectors per track, and
/// every tool writes the largest address it can hold for a sector past it,
/// which is about 8 GB. Firmware that reads CHS at all expects exactly that.
fn chs(lba: u64) -> [u8; 3] {
    const HEADS: u64 = 255;
    const SECTORS: u64 = 63;
    let cylinder = lba / (HEADS * SECTORS);
    if cylinder > 1023 {
        return [0xFE, 0xFF, 0xFF];
    }
    let head = (lba / SECTORS) % HEADS;
    let sector = lba % SECTORS + 1;
    [
        head as u8,
        (sector as u8) | (((cylinder >> 2) & 0xC0) as u8),
        (cylinder & 0xFF) as u8,
    ]
}

/// Write the table, after taking away every trace of the one before.
///
/// Three things survive a new MBR if nobody removes them:
///
/// - A GPT keeps its header at sector 1 and a copy of it in the last sectors
///   of the drive. Firmware and `gdisk` then see a GPT over this table. The
///   first mebibyte and the last mebibyte of the drive go to zero, which
///   covers both at either sector size.
/// - A file system keeps its signature near its first byte. A host would
///   mount what was there before in a partition that holds nothing yet. The
///   first mebibyte of each partition goes to zero.
/// - The table itself goes last, so a drive stopped half way has no table
///   that points at old data.
///
/// Every write is whole sectors at a sector offset, so a raw device takes it
/// with no adapter in between.
///
/// Call this before either partition gets its file system. On a drive that
/// is not whole mebibytes, the last mebibyte reaches into the end of
/// partition 2.
pub fn write_table<T: BlockTarget>(
    target: &mut T,
    layout: &Layout,
    disk_signature: u32,
) -> Result<()> {
    let zeros = vec![0u8; ALIGN as usize];
    // The last mebibyte of the drive, and not the last whole mebibyte. A
    // drive is rarely a whole number of mebibytes, and the copy of the GPT
    // header sits in its very last sector.
    let last = layout.drive_bytes.saturating_sub(ALIGN);
    for at in [0, layout.boot.start, layout.install.start, last] {
        target.seek(SeekFrom::Start(at))?;
        target.write_all(&zeros)?;
    }

    let mut first = vec![0u8; layout.sector_size as usize];
    first[..512].copy_from_slice(&mbr_bytes(layout, disk_signature));
    target.seek(SeekFrom::Start(0))?;
    target.write_all(&first)?;
    target.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan;
    use burnout_core::{has_boot_table, MemoryTarget, StrictTarget};
    use std::io::{Seek, Write};

    const MIB: u64 = 1024 * 1024;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_table_carries_the_signature_and_both_partitions() {
        let l = plan(64 * MIB, 512, 16 * MIB).unwrap();
        let mbr = mbr_bytes(&l, 0x1234_5678);
        assert_eq!(&mbr[510..], &[0x55, 0xAA]);
        assert!(has_boot_table(&mbr), "the table reads as one");
        assert_eq!(u32_at(&mbr, 440), 0x1234_5678);

        assert_eq!(mbr[446], 0x80, "partition 1 is active");
        assert_eq!(mbr[446 + 4], TYPE_FAT32);
        assert_eq!(u32_at(&mbr, 446 + 8), 2048);
        assert_eq!(u32_at(&mbr, 446 + 12) as u64, 16 * MIB / 512);

        assert_eq!(mbr[462], 0x00, "partition 2 is not");
        assert_eq!(mbr[462 + 4], TYPE_EXFAT);
        assert_eq!(u32_at(&mbr, 462 + 8) as u64, 17 * MIB / 512);
    }

    #[test]
    fn a_4096_byte_drive_counts_its_own_sectors() {
        // The table does not count bytes and it does not count 512-byte
        // blocks. It counts the sectors of the drive it describes.
        let l = plan(64 * MIB, 4096, 16 * MIB).unwrap();
        let mbr = mbr_bytes(&l, 1);
        assert_eq!(u32_at(&mbr, 446 + 8), 256);
        assert_eq!(u32_at(&mbr, 446 + 12) as u64, 16 * MIB / 4096);
    }

    #[test]
    fn the_address_of_sector_2048_is_the_one_every_tool_writes() {
        assert_eq!(chs(2048), [0x20, 0x21, 0x00]);
        assert_eq!(chs(0), [0x00, 0x01, 0x00]);
    }

    #[test]
    fn an_address_past_eight_gigabytes_is_the_largest_one() {
        assert_eq!(chs(1024 * 255 * 63), [0xFE, 0xFF, 0xFF]);
        assert_eq!(chs(u32::MAX as u64), [0xFE, 0xFF, 0xFF]);
    }

    /// A drive that held a GPT disk and a file system, the way a drive does
    /// after a Linux image was written to it.
    fn used_drive(sector: u32) -> StrictTarget<MemoryTarget> {
        let size = 64 * MIB;
        let mut drive = MemoryTarget::new(size, sector).unwrap();
        drive.write_all(&vec![0xAA; size as usize]).unwrap();
        let header = b"EFI PART";
        for at in [sector as u64, size - sector as u64] {
            drive.seek(SeekFrom::Start(at)).unwrap();
            drive.write_all(header).unwrap();
        }
        StrictTarget::new(drive)
    }

    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn an_old_gpt_goes_at_both_ends_of_the_drive() {
        for sector in [512, 4096] {
            let l = plan(64 * MIB, sector, 16 * MIB).unwrap();
            let mut drive = used_drive(sector);
            write_table(&mut drive, &l, 7).unwrap();
            let bytes = drive.inner().contents();
            assert!(
                !contains(bytes, b"EFI PART"),
                "a GPT header survived at {sector}"
            );
            assert!(has_boot_table(&bytes[..512]));
        }
    }

    #[test]
    fn the_last_sector_of_a_drive_that_is_not_whole_mebibytes_is_cleaned() {
        // 64 MiB and three sectors. A wipe of the last whole mebibyte would
        // stop three sectors short of the copy of the GPT header.
        for sector in [512u32, 4096] {
            let size = 64 * MIB + 3 * sector as u64;
            let mut drive = MemoryTarget::new(size, sector).unwrap();
            drive.seek(SeekFrom::Start(size - sector as u64)).unwrap();
            drive.write_all(b"EFI PART").unwrap();
            let mut drive = StrictTarget::new(drive);

            let l = plan(size, sector, 16 * MIB).unwrap();
            write_table(&mut drive, &l, 7).unwrap();
            assert!(!contains(drive.inner().contents(), b"EFI PART"));
        }
    }

    #[test]
    fn the_start_of_each_partition_is_clean_and_the_rest_is_untouched() {
        // Zero where an old file system keeps its signature, and nowhere
        // else. Zeroing a whole drive would take an hour on a slow stick.
        let l = plan(64 * MIB, 512, 16 * MIB).unwrap();
        let mut drive = used_drive(512);
        write_table(&mut drive, &l, 7).unwrap();
        let bytes = drive.inner().contents();

        for start in [l.boot.start, l.install.start] {
            let head = &bytes[start as usize..(start + MIB) as usize];
            assert!(head.iter().all(|b| *b == 0));
        }
        let middle = (l.install.start + 8 * MIB) as usize;
        assert!(bytes[middle..middle + 4096].iter().all(|b| *b == 0xAA));
    }

    #[test]
    fn every_write_of_the_table_is_whole_sectors() {
        // The strict drive refuses anything else, so a pass here is a pass
        // on a raw device.
        for sector in [512, 4096] {
            let l = plan(64 * MIB, sector, 16 * MIB).unwrap();
            let mut drive = StrictTarget::new(MemoryTarget::new(64 * MIB, sector).unwrap());
            write_table(&mut drive, &l, 7).unwrap();
        }
    }
}
