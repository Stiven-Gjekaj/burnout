//! Where each partition of the Windows layout starts, and how long it is.
//!
//! This is arithmetic and nothing else. It opens no drive and writes no byte,
//! so every rule here is tested on three hosts with no drive near the test.

use burnout_core::{check_aligned, check_sector_size, Error, Result};

/// Every partition starts and ends on a mebibyte.
///
/// That is what Windows, macOS and Linux all do, and it is a whole number of
/// sectors on every drive, 512 or 4096. A partition that starts inside an
/// erase block of a flash drive makes every write touch two of them.
pub const ALIGN: u64 = 1024 * 1024;

/// An MBR counts sectors in 32 bits, for the start and for the length.
///
/// So the table reaches 2 TiB on a drive of 512-byte sectors and 16 TiB on a
/// drive of 4096-byte ones. Past that the table cannot name a byte.
pub const MBR_SECTORS: u64 = 1 << 32;

/// A range of bytes on the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    /// The first byte.
    pub start: u64,
    /// How many bytes.
    pub length: u64,
}

impl Extent {
    /// One byte past the last.
    pub fn end(&self) -> u64 {
        self.start + self.length
    }
}

/// The two partitions of a Windows drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The logical sector size of the drive, in bytes.
    pub sector_size: u32,
    /// The size of the drive, in bytes.
    pub drive_bytes: u64,
    /// Partition 1, FAT32, with every boot file.
    pub boot: Extent,
    /// Partition 2, exFAT, with the install image.
    pub install: Extent,
    /// The bytes past the last byte that an MBR can name.
    ///
    /// Zero on any drive under 2 TiB. The person should hear about it when it
    /// is not zero, because the drive is larger than what they get.
    pub beyond_table: u64,
}

impl Layout {
    /// The first sector of an extent, as the table counts it.
    pub fn first_sector(&self, extent: Extent) -> u64 {
        extent.start / self.sector_size as u64
    }

    /// The length of an extent in sectors, as the table counts it.
    pub fn sectors(&self, extent: Extent) -> u64 {
        extent.length / self.sector_size as u64
    }
}

/// Lay out a drive of `drive_bytes` with a boot partition of at least
/// `boot_bytes`.
///
/// Partition 1 starts at 1 MiB and holds `boot_bytes`, grown to a whole
/// mebibyte. Partition 2 starts where partition 1 ends and takes the rest of
/// the drive, down to the last whole mebibyte, and down to the last byte that
/// the table can name.
pub fn plan(drive_bytes: u64, sector_size: u32, boot_bytes: u64) -> Result<Layout> {
    let sector_size = check_sector_size(sector_size)?;
    check_aligned(drive_bytes, sector_size)?;

    let boot_length = round_up(boot_bytes.max(1), ALIGN);
    let boot = Extent {
        start: ALIGN,
        length: boot_length,
    };

    let table_end = MBR_SECTORS * sector_size as u64;
    let usable_end = drive_bytes.min(table_end);
    let install_start = boot.end();
    let install_end = round_down(usable_end, ALIGN);

    // Partition 2 needs at least one mebibyte of its own, or there is no
    // room for the install image at all.
    if install_end < install_start + ALIGN {
        return Err(Error::TooSmall {
            image_bytes: install_start + ALIGN,
            drive_bytes,
        });
    }

    Ok(Layout {
        sector_size,
        drive_bytes,
        boot,
        install: Extent {
            start: install_start,
            length: install_end - install_start,
        },
        beyond_table: drive_bytes.saturating_sub(table_end),
    })
}

fn round_up(value: u64, unit: u64) -> u64 {
    value.div_ceil(unit) * unit
}

fn round_down(value: u64, unit: u64) -> u64 {
    value / unit * unit
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn partition_1_starts_at_one_mebibyte_on_either_sector_size() {
        // The table counts sectors, so the same byte is sector 2048 on one
        // drive and sector 256 on a 4Kn drive.
        let small = plan(32 * GB, 512, 1400 * MIB).unwrap();
        let large = plan(32 * GB / 4096 * 4096, 4096, 1400 * MIB).unwrap();
        assert_eq!(small.first_sector(small.boot), 2048);
        assert_eq!(large.first_sector(large.boot), 256);
    }

    #[test]
    fn the_boot_partition_grows_to_a_whole_mebibyte() {
        let l = plan(32 * GB, 512, 1_000_000_000).unwrap();
        assert_eq!(l.boot.length % MIB, 0);
        assert!(l.boot.length >= 1_000_000_000);
        assert!(l.boot.length < 1_000_000_000 + MIB);
    }

    #[test]
    fn partition_2_starts_where_partition_1_ends_and_ends_on_a_mebibyte() {
        let l = plan(32 * GB, 512, 1400 * MIB).unwrap();
        assert_eq!(l.install.start, l.boot.end());
        assert_eq!(l.install.start % MIB, 0);
        assert_eq!(l.install.end() % MIB, 0);
        assert!(l.install.end() <= l.drive_bytes);
        assert!(l.drive_bytes - l.install.end() < MIB);
    }

    #[test]
    fn a_drive_past_two_tebibytes_stops_where_the_table_stops() {
        // 3 TB of 512-byte sectors is more than 32 bits of sectors. The
        // table cannot name the tail, so the plan says how much is lost.
        let drive = 3_000_000_000_000u64 / 512 * 512;
        let l = plan(drive, 512, 1400 * MIB).unwrap();
        assert!(l.install.end() <= MBR_SECTORS * 512);
        assert_eq!(l.beyond_table, drive - MBR_SECTORS * 512);
        assert!(l.first_sector(l.install) + l.sectors(l.install) <= MBR_SECTORS);
    }

    #[test]
    fn the_same_drive_on_4096_byte_sectors_loses_nothing() {
        let drive = 3_000_000_000_000u64 / 4096 * 4096;
        let l = plan(drive, 4096, 1400 * MIB).unwrap();
        assert_eq!(l.beyond_table, 0);
    }

    #[test]
    fn a_drive_with_no_room_for_partition_2_is_refused() {
        assert!(matches!(
            plan(1400 * MIB + MIB, 512, 1400 * MIB),
            Err(Error::TooSmall { .. })
        ));
    }

    #[test]
    fn a_drive_whose_size_is_not_whole_sectors_is_refused() {
        // A drive always is whole sectors. A size that is not came from a
        // host that this code read wrongly.
        assert!(plan(32 * GB + 100, 512, 1400 * MIB).is_err());
        assert!(plan(32 * GB, 1024, 1400 * MIB).is_err());
    }
}
