//! The boot region of an exFAT volume.
//!
//! Twelve sectors: the boot sector, eight extended boot sectors, the OEM
//! parameters, a reserved sector, and a sector that repeats the checksum of
//! the eleven before it. A copy of all twelve follows at sector 12, and a
//! host turns to it when the first is damaged.

use super::geometry::Geometry;
use super::sums::boot_checksum;

/// The sectors of one boot region.
pub(crate) const REGION_SECTORS: u64 = 12;

/// The sectors that the checksum covers.
const SUMMED_SECTORS: usize = 11;

/// The first bytes of a boot sector: a jump over the fields, which no host
/// runs, and the name of the file system.
const JUMP_AND_NAME: &[u8; 11] = b"\xEB\x76\x90EXFAT   ";

/// Revision 1.00, the one that the specification describes.
const REVISION: u16 = 0x0100;

/// The number of the drive for BIOS, as every FAT file system writes it.
const DRIVE_SELECT: u8 = 0x80;

/// The instruction that stops the CPU. It fills the boot code, because this
/// volume starts no system from its boot sector.
const HALT: u8 = 0xF4;

/// What the boot sector records beside the geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BootFields {
    /// The first sector of the partition, counted from the start of the drive.
    pub partition_offset: u64,
    /// The volume serial number.
    pub serial: u32,
    /// The first cluster of the root directory.
    pub root_cluster: u32,
    /// The part of the cluster heap in use, in percent, rounded down.
    pub percent_in_use: u8,
}

/// The bytes of one boot region, which are also the bytes of its copy.
pub(crate) fn boot_region(g: &Geometry, fields: &BootFields) -> Vec<u8> {
    let sector = g.sector_bytes as usize;
    let mut region = vec![0u8; REGION_SECTORS as usize * sector];

    let boot = &mut region[..sector];
    boot[..11].copy_from_slice(JUMP_AND_NAME);
    // Bytes 11 to 63 stay zero, where FAT keeps its parameters, so no FAT
    // driver takes the volume for its own.
    boot[64..72].copy_from_slice(&fields.partition_offset.to_le_bytes());
    boot[72..80].copy_from_slice(&g.volume_sectors.to_le_bytes());
    boot[80..84].copy_from_slice(&g.fat_offset.to_le_bytes());
    boot[84..88].copy_from_slice(&g.fat_length.to_le_bytes());
    boot[88..92].copy_from_slice(&g.heap_offset.to_le_bytes());
    boot[92..96].copy_from_slice(&g.cluster_count.to_le_bytes());
    boot[96..100].copy_from_slice(&fields.root_cluster.to_le_bytes());
    boot[100..104].copy_from_slice(&fields.serial.to_le_bytes());
    boot[104..106].copy_from_slice(&REVISION.to_le_bytes());
    // The volume flags at 106 stay zero: the first FAT, and a clean volume.
    boot[108] = g.sector_shift();
    boot[109] = g.cluster_shift();
    boot[110] = 1;
    boot[111] = DRIVE_SELECT;
    boot[112] = fields.percent_in_use;
    boot[120..510].fill(HALT);
    boot[510] = 0x55;
    boot[511] = 0xAA;

    // Each extended boot sector ends with its signature, AA550000h, at the
    // end of the sector and not at byte 510.
    for number in 1..=8 {
        let end = (number + 1) * sector;
        region[end - 4..end].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    }
    // The OEM parameters and the reserved sector stay zero. Ten parameters
    // of zero are ten parameters that say nothing.

    let sum = boot_checksum(&region[..SUMMED_SECTORS * sector]);
    for word in region[SUMMED_SECTORS * sector..].chunks_exact_mut(4) {
        word.copy_from_slice(&sum.to_le_bytes());
    }
    region
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn u64_at(bytes: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
    }

    /// The volume of 64 MiB that `newfs_exfat` of macOS 26 made, with the
    /// geometry and the fields that its boot sector holds.
    fn macos() -> (Geometry, BootFields) {
        let g = Geometry {
            sector_bytes: 512,
            cluster_sectors: 8,
            volume_sectors: 131_072,
            fat_offset: 128,
            fat_length: 128,
            heap_offset: 256,
            cluster_count: 16_352,
        };
        let fields = BootFields {
            partition_offset: 0,
            serial: 0x6AB3_0E6A,
            root_cluster: 5,
            percent_in_use: 0,
        };
        (g, fields)
    }

    #[test]
    fn the_region_for_the_volume_of_macos_has_the_checksum_that_macos_wrote() {
        // The checksum covers every byte of the eleven sectors. A field in
        // the wrong place, or a signature in the wrong sector, changes it.
        let (g, fields) = macos();
        let region = boot_region(&g, &fields);
        for word in region[11 * 512..].chunks_exact(4) {
            assert_eq!(u32::from_le_bytes(word.try_into().unwrap()), 0x411E_F78F);
        }
    }

    #[test]
    fn the_boot_sector_holds_the_geometry() {
        for sector in [512u32, 4096] {
            let g = Geometry::new(64 * 1024 * 1024, sector).unwrap();
            let fields = BootFields {
                partition_offset: 12_345,
                serial: 0xB0B0_CAFE,
                root_cluster: 4,
                percent_in_use: 3,
            };
            let region = boot_region(&g, &fields);
            let boot = &region[..sector as usize];
            assert_eq!(&boot[..11], JUMP_AND_NAME);
            assert!(boot[11..64].iter().all(|b| *b == 0));
            assert_eq!(u64_at(boot, 64), 12_345);
            assert_eq!(u64_at(boot, 72), g.volume_sectors);
            assert_eq!(u32_at(boot, 80), g.fat_offset);
            assert_eq!(u32_at(boot, 84), g.fat_length);
            assert_eq!(u32_at(boot, 88), g.heap_offset);
            assert_eq!(u32_at(boot, 92), g.cluster_count);
            assert_eq!(u32_at(boot, 96), 4);
            assert_eq!(u32_at(boot, 100), 0xB0B0_CAFE);
            assert_eq!(&boot[104..108], &[0x00, 0x01, 0x00, 0x00]);
            assert_eq!(1u32 << boot[108], sector);
            assert_eq!(1u64 << boot[109], g.cluster_bytes() / sector as u64);
            assert_eq!(&boot[110..113], &[1, 0x80, 3]);
            assert!(boot[120..510].iter().all(|b| *b == HALT));
            assert_eq!(&boot[510..512], &[0x55, 0xAA]);
            // A 4096-byte sector keeps nothing past its first 512 bytes.
            assert!(boot[512..].iter().all(|b| *b == 0));
        }
    }

    #[test]
    fn each_extended_sector_ends_with_its_signature() {
        for sector in [512usize, 4096] {
            let g = Geometry::new(64 * 1024 * 1024, sector as u32).unwrap();
            let region = boot_region(&g, &macos().1);
            for number in 1..=8 {
                let s = &region[number * sector..(number + 1) * sector];
                assert_eq!(&s[sector - 4..], &[0x00, 0x00, 0x55, 0xAA]);
                assert!(s[..sector - 4].iter().all(|b| *b == 0));
            }
            let oem_and_reserved = &region[9 * sector..11 * sector];
            assert!(oem_and_reserved.iter().all(|b| *b == 0));
        }
    }

    #[test]
    fn the_last_sector_repeats_the_checksum_of_the_eleven_before_it() {
        for sector in [512usize, 4096] {
            let g = Geometry::new(64 * 1024 * 1024, sector as u32).unwrap();
            let region = boot_region(&g, &macos().1);
            let sum = boot_checksum(&region[..11 * sector]);
            let last = &region[11 * sector..];
            assert_eq!(last.len(), sector);
            assert!(last
                .chunks_exact(4)
                .all(|w| u32::from_le_bytes(w.try_into().unwrap()) == sum));
        }
    }
}
