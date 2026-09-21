//! The FAT32 volume on partition 1, which holds every boot file.
//!
//! `fatfs` formats the volume. It reads and writes where it likes, four bytes
//! at a time for an entry of the table, so it runs over a [`SectorIo`] that
//! turns each access into whole sectors. That is why the same code can write
//! a file in a test and a raw device in the product.

use std::io::{self, Read, Seek, SeekFrom, Write};

use burnout_core::{BlockTarget, Error, Result, SectorIo};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};

/// A volume with fewer clusters than this is FAT16, whatever its boot sector
/// says, because every host decides the type from the count.
const MIN_CLUSTERS: u64 = 65_525;

/// The largest cluster that this picks.
///
/// Windows picks 4 KiB for a volume from 256 MB to 8 GB, which is where a
/// boot partition falls.
const MAX_CLUSTER_BYTES: u32 = 4096;

/// `fatfs` reserves 8 sectors at the start of a FAT32 volume. They hold the
/// boot sector, the sector that counts free space, and a copy of the boot
/// sector.
const RESERVED_SECTORS: u64 = 8;

/// Where the boot sector keeps how many sectors of the drive come before the
/// volume.
const HIDDEN_SECTORS_AT: u64 = 28;

/// Where the boot sector keeps the sector of its own copy.
const BACKUP_SECTOR_AT: u64 = 50;

/// What a new FAT32 volume is called, and where it sits on the drive.
#[derive(Clone, Copy, Debug)]
pub struct Fat32Options<'a> {
    /// The name that a host shows for the volume.
    ///
    /// FAT32 keeps 11 characters of it, in upper case. A character that a
    /// label cannot hold becomes `_`. An empty label gives a volume with no
    /// label.
    pub label: &'a str,
    /// The volume serial number.
    ///
    /// Windows shows it and tells volumes apart by it, so the caller gives a
    /// number of its own for each drive.
    pub serial: u32,
    /// The sector of the drive where the partition starts, as the partition
    /// table counts it.
    pub first_sector: u32,
}

/// Format the whole of `volume` as FAT32.
///
/// `volume` is a partition in the product, and anything that takes whole
/// sectors in a test. Every access to it is whole sectors.
///
/// A partition too small for FAT32 is refused. It is not formatted as FAT16,
/// because the drive would then carry a file system that nobody asked for.
pub fn format_fat32<T: BlockTarget>(mut volume: T, options: &Fat32Options) -> Result<()> {
    let sector_size = volume.logical_sector_size();
    let partition_bytes = volume.length();
    let sectors = partition_bytes / sector_size as u64;
    let cluster = cluster_bytes(sectors, sector_size).ok_or(Error::PartitionTooSmall {
        file_system: "FAT32",
        partition_bytes,
        needed_bytes: min_sectors(sector_size) * sector_size as u64,
    })?;
    let total = u32::try_from(sectors).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "FAT32 counts sectors in 32 bits, and the partition holds more",
        )
    })?;

    let mut format = FormatVolumeOptions::new()
        .bytes_per_sector(sector_size as u16)
        .total_sectors(total)
        .bytes_per_cluster(cluster)
        // The geometry that the partition table uses.
        .sectors_per_track(63)
        .heads(255)
        .volume_id(options.serial);
    if let Some(label) = label_bytes(options.label) {
        format = format.volume_label(label);
    }

    over_sectors(&mut volume, |io| {
        fatfs::format_volume(&mut *io, format)?;
        record_first_sector(io, sector_size, options.first_sector)?;
        Ok(())
    })?;

    // Mount the new volume once, which also proves that it is FAT32, and
    // count its free clusters. `fatfs` leaves the count unknown after a
    // format, and a host then counts the whole table the first time it needs
    // the number.
    with_volume(&mut volume, |fs| {
        fs.stats()?;
        Ok(())
    })
}

/// A mounted FAT32 volume, as `fatfs` holds it.
pub(crate) type Volume<'a, T> = FileSystem<&'a mut SectorIo<T>>;

/// The date and time of every new entry: the first second that FAT holds.
///
/// Without `chrono`, `fatfs` writes a date of zero. That is not a date,
/// because FAT counts the month and the day from one, and macOS shows it as
/// 1970. One fixed time also keeps one tree one volume, to the byte.
#[derive(Debug)]
struct FatEpoch;

impl fatfs::TimeProvider for FatEpoch {
    fn get_current_date(&self) -> fatfs::Date {
        fatfs::Date {
            year: 1980,
            month: 1,
            day: 1,
        }
    }

    fn get_current_date_time(&self) -> fatfs::DateTime {
        fatfs::DateTime {
            date: self.get_current_date(),
            time: fatfs::Time {
                hour: 0,
                min: 0,
                sec: 0,
                millis: 0,
            },
        }
    }
}

static FAT_EPOCH: FatEpoch = FatEpoch;

/// Mount `volume`, give it to `work`, and unmount it whatever `work` returns.
///
/// A volume that is not FAT32 is refused before `work` runs.
pub(crate) fn with_volume<T: BlockTarget, R>(
    volume: T,
    work: impl FnOnce(&Volume<'_, T>) -> Result<R>,
) -> Result<R> {
    over_sectors(volume, |io| mounted(io, work))
}

/// The same, over a [`SectorIo`] that the caller keeps for more work after
/// the unmount.
pub(crate) fn mounted<T: BlockTarget, R>(
    io: &mut SectorIo<T>,
    work: impl FnOnce(&Volume<'_, T>) -> Result<R>,
) -> Result<R> {
    io.seek(SeekFrom::Start(0))?;
    let options = FsOptions::new().time_provider(&FAT_EPOCH);
    let fs = FileSystem::new(&mut *io, options)?;
    if fs.fat_type() != FatType::Fat32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the volume is {:?} and not FAT32", fs.fat_type()),
        )
        .into());
    }
    let result = work(&fs);
    let unmounted = fs.unmount();
    let value = result?;
    unmounted?;
    Ok(value)
}

/// Give `work` a [`SectorIo`] over `volume`, and flush it whatever `work`
/// returns.
///
/// A drop writes the cache back too, but it cannot report an error. An error
/// from `work` comes first, because it is the cause.
pub(crate) fn over_sectors<T: BlockTarget, R>(
    volume: T,
    work: impl FnOnce(&mut SectorIo<T>) -> Result<R>,
) -> Result<R> {
    let mut io = SectorIo::new(volume);
    let result = work(&mut io);
    let flushed = io.flush();
    let value = result?;
    flushed?;
    Ok(value)
}

/// Write where the partition starts into the boot sector and into its copy.
///
/// Boot code that a BIOS runs adds this number to each sector of the volume
/// to find that sector on the drive. `fatfs` does not know the partition, so
/// it writes zero.
fn record_first_sector<T: BlockTarget>(
    io: &mut SectorIo<T>,
    sector_size: u32,
    first_sector: u32,
) -> io::Result<()> {
    let mut backup = [0u8; 2];
    io.seek(SeekFrom::Start(BACKUP_SECTOR_AT))?;
    io.read_exact(&mut backup)?;
    let backup = u16::from_le_bytes(backup) as u64 * sector_size as u64;
    for boot_sector in [0, backup] {
        io.seek(SeekFrom::Start(boot_sector + HIDDEN_SECTORS_AT))?;
        io.write_all(&first_sector.to_le_bytes())?;
    }
    Ok(())
}

/// The label as FAT32 keeps it: 11 bytes, in upper case, padded with spaces.
fn label_bytes(label: &str) -> Option<[u8; 11]> {
    let mut bytes = [b' '; 11];
    let mut count = 0;
    for c in label.trim().chars().take(bytes.len()) {
        bytes[count] = match c.to_ascii_uppercase() {
            c @ ('A'..='Z'
            | '0'..='9'
            | ' '
            | '!'
            | '#'
            | '$'
            | '%'
            | '&'
            | '\''
            | '('
            | ')'
            | '-'
            | '@'
            | '^'
            | '_'
            | '`'
            | '{'
            | '}'
            | '~') => c as u8,
            _ => b'_',
        };
        count += 1;
    }
    (count > 0).then_some(bytes)
}

/// The cluster size for a volume of `sectors` sectors, in bytes.
///
/// It starts at [`MAX_CLUSTER_BYTES`] and halves while the volume would hold
/// too few clusters to be FAT32. `None` when one sector for each cluster is
/// still too few.
fn cluster_bytes(sectors: u64, sector_size: u32) -> Option<u32> {
    let mut bytes = MAX_CLUSTER_BYTES.max(sector_size);
    loop {
        if clusters(sectors, sector_size, bytes / sector_size) >= MIN_CLUSTERS {
            return Some(bytes);
        }
        if bytes == sector_size {
            return None;
        }
        bytes /= 2;
    }
}

/// The fewest sectors that hold a FAT32 volume.
fn min_sectors(sector_size: u32) -> u64 {
    let mut sectors = RESERVED_SECTORS + MIN_CLUSTERS;
    while clusters(sectors, sector_size, 1) < MIN_CLUSTERS {
        sectors += 1;
    }
    sectors
}

/// How many clusters a FAT32 volume of `sectors` sectors holds.
///
/// This is the arithmetic of `fatfs`. The reserved sectors come first. Two
/// copies of the table come next, each large enough for an entry of 4 bytes
/// for each cluster and for the two entries before the first cluster. The
/// clusters take the rest. A test formats a volume at the exact minimum that
/// this gives, so a change in `fatfs` cannot make the two disagree unseen.
fn clusters(sectors: u64, sector_size: u32, per_cluster: u32) -> u64 {
    let per_cluster = per_cluster as u64;
    let entries_per_sector = sector_size as u64 / 4;
    let unreserved = sectors.saturating_sub(RESERVED_SECTORS);
    let table = (unreserved + 2 * per_cluster).div_ceil(per_cluster * entries_per_sector + 2);
    sectors.saturating_sub(RESERVED_SECTORS + 2 * table) / per_cluster
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Drive, MIB, SERIAL};

    const GIB: u64 = 1024 * MIB;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn the_volume_mounts_as_fat32_with_its_label_and_serial() {
        // The strict drive refuses any access that is not whole sectors, so
        // this also proves that a raw device takes the format.
        for sector in [512, 4096] {
            let mut d = Drive::usual(sector);
            d.format().unwrap();
            with_volume(d.partition(), |fs| {
                assert_eq!(fs.fat_type(), FatType::Fat32);
                assert_eq!(fs.volume_label(), "BURNOUT");
                assert_eq!(
                    fs.read_volume_label_from_root_dir()?.as_deref(),
                    Some("BURNOUT")
                );
                assert_eq!(fs.volume_id(), SERIAL);
                assert_eq!(fs.cluster_size(), sector);
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn both_boot_sectors_record_where_the_partition_starts() {
        for sector in [512u32, 4096] {
            let mut d = Drive::usual(sector);
            d.format().unwrap();
            let p = d.partition_bytes();
            let s = sector as usize;
            assert_eq!(u32_at(p, 28) as u64, MIB / sector as u64);
            assert_eq!(&p[..s], &p[6 * s..7 * s], "the copy is the same");
        }
    }

    #[test]
    fn the_volume_is_clean_and_knows_how_much_of_it_is_free() {
        for sector in [512u32, 4096] {
            let mut d = Drive::usual(sector);
            d.format().unwrap();
            // The count of free clusters, in the second sector.
            let recorded = u32_at(d.partition_bytes(), sector as usize + 488);
            assert_ne!(recorded, u32::MAX, "the count is known");
            with_volume(d.partition(), |fs| {
                assert!(!fs.read_status_flags()?.dirty());
                let stats = fs.stats()?;
                assert_eq!(stats.free_clusters(), recorded);
                // The root directory takes one cluster. Nothing else does.
                assert_eq!(stats.free_clusters(), stats.total_clusters() - 1);
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn nothing_outside_the_partition_changes() {
        let mut d = Drive::new(512, 40 * MIB);
        let size = d.bytes().len();
        d.strict.write_all(&vec![0xAA; size]).unwrap();
        d.format().unwrap();
        let bytes = d.bytes();
        assert!(bytes[..MIB as usize].iter().all(|b| *b == 0xAA));
        assert!(bytes[(41 * MIB) as usize..].iter().all(|b| *b == 0xAA));
    }

    #[test]
    fn a_boot_partition_gets_clusters_of_4_kib() {
        // About 1 GB is the size of partition 1 for a Windows image.
        assert_eq!(cluster_bytes(GIB / 512, 512), Some(4096));
        assert_eq!(cluster_bytes(GIB / 4096, 4096), Some(4096));
    }

    #[test]
    fn a_smaller_partition_gets_smaller_clusters_to_stay_fat32() {
        assert_eq!(cluster_bytes(200 * MIB / 512, 512), Some(2048));
        assert_eq!(cluster_bytes(100 * MIB / 512, 512), Some(1024));
        assert_eq!(cluster_bytes(40 * MIB / 512, 512), Some(512));
        // A 4Kn drive has no sector smaller than 4096 to give.
        assert_eq!(cluster_bytes(200 * MIB / 4096, 4096), None);
    }

    #[test]
    fn the_smallest_fat32_partition_formats_and_one_sector_less_is_refused() {
        for sector in [512u32, 4096] {
            let smallest = min_sectors(sector) * sector as u64;

            let mut d = Drive::new(sector, smallest);
            d.format().unwrap();
            with_volume(d.partition(), |fs| {
                assert_eq!(fs.fat_type(), FatType::Fat32);
                Ok(())
            })
            .unwrap();

            let mut d = Drive::new(sector, smallest - sector as u64);
            match d.format() {
                Err(Error::PartitionTooSmall { needed_bytes, .. }) => {
                    assert_eq!(needed_bytes, smallest)
                }
                other => panic!("{sector}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_count_of_clusters_is_the_count_that_fatfs_gives() {
        // At these sizes the table rounds up by one sector. Arithmetic that
        // rounds the other way counts two clusters too many here.
        for (sector, sectors) in [(512u32, 81_907u64), (4096, 65_671)] {
            let mut d = Drive::new(sector, sectors * sector as u64);
            d.format().unwrap();
            with_volume(d.partition(), |fs| {
                let counted = fs.stats()?.total_clusters() as u64;
                assert_eq!(counted, clusters(sectors, sector, 1));
                Ok(())
            })
            .unwrap();
        }
    }

    /// Format with fatfs alone, and report whether the result is FAT32.
    fn fatfs_makes_fat32(sector: u32, length: u64) -> bool {
        let mut d = Drive::new(sector, length);
        let made = over_sectors(d.partition(), |io| {
            let options = FormatVolumeOptions::new()
                .bytes_per_sector(sector as u16)
                .bytes_per_cluster(sector);
            fatfs::format_volume(&mut *io, options)?;
            Ok(())
        });
        made.is_ok() && with_volume(d.partition(), |_| Ok(())).is_ok()
    }

    #[test]
    fn fatfs_puts_the_smallest_fat32_partition_where_this_does() {
        // The refusal above follows this arithmetic, and the arithmetic is a
        // copy of what fatfs does. Ask fatfs itself, so the two cannot drift
        // apart. One sector less, fatfs refuses or makes something else.
        for sector in [512u32, 4096] {
            let smallest = min_sectors(sector) * sector as u64;
            assert!(fatfs_makes_fat32(sector, smallest));
            assert!(!fatfs_makes_fat32(sector, smallest - sector as u64));
        }
    }

    #[test]
    fn a_volume_that_is_not_fat32_is_refused_at_mount() {
        // 16 MiB is FAT16 when fatfs chooses.
        let mut d = Drive::new(512, 16 * MIB);
        over_sectors(d.partition(), |io| {
            fatfs::format_volume(&mut *io, FormatVolumeOptions::new())?;
            Ok(())
        })
        .unwrap();
        let e = with_volume(d.partition(), |_| Ok(())).unwrap_err();
        assert!(e.to_string().contains("Fat16"), "{e}");
    }

    #[test]
    fn a_label_keeps_only_what_fat32_can_hold() {
        assert_eq!(label_bytes("win 11 x64 boot"), Some(*b"WIN 11 X64 "));
        assert_eq!(label_bytes("a/b.c"), Some(*b"A_B_C      "));
        assert_eq!(label_bytes("\u{fc}ber"), Some(*b"_BER       "));
        assert_eq!(label_bytes("  "), None);
    }
}
