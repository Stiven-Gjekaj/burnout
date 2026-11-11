//! The boot sector of an NTFS volume, and its copy in the last sector.

use crate::ntfs::record::RECORD_BYTES;

/// The shape of a volume, as the boot sector gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub sector_bytes: u32,
    pub cluster_bytes: u32,
    /// The sectors of the partition. The last one holds the copy of the boot
    /// sector, and the volume does not count it.
    pub partition_sectors: u64,
    /// The first sector of the partition on the drive.
    pub hidden_sectors: u32,
    pub mft_cluster: u64,
    pub mirror_cluster: u64,
    /// The bytes of one block of an index.
    pub index_bytes: u32,
    pub serial: u64,
}

/// The size field of the boot sector for `bytes`: the count of clusters
/// when a record fills one cluster or more, else the negative power of two.
fn size_field(bytes: u32, cluster_bytes: u32) -> u8 {
    if bytes >= cluster_bytes {
        (bytes / cluster_bytes) as u8
    } else {
        (-(bytes.trailing_zeros() as i8)) as u8
    }
}

/// The 512 bytes of the boot sector. The sector holds no boot code: a drive
/// of Windows To Go starts through the boot files on its EFI partition, and
/// the code that says so takes the place of the boot code.
pub(crate) fn boot_sector(g: &Geometry) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    b[3..11].copy_from_slice(b"NTFS    ");
    b[0x0B..0x0D].copy_from_slice(&(g.sector_bytes as u16).to_le_bytes());
    b[0x0D] = (g.cluster_bytes / g.sector_bytes) as u8;
    b[0x15] = 0xF8;
    b[0x18..0x1A].copy_from_slice(&63u16.to_le_bytes());
    b[0x1A..0x1C].copy_from_slice(&255u16.to_le_bytes());
    b[0x1C..0x20].copy_from_slice(&g.hidden_sectors.to_le_bytes());
    b[0x24..0x28].copy_from_slice(&0x0080_0080u32.to_le_bytes());
    b[0x28..0x30].copy_from_slice(&(g.partition_sectors - 1).to_le_bytes());
    b[0x30..0x38].copy_from_slice(&g.mft_cluster.to_le_bytes());
    b[0x38..0x40].copy_from_slice(&g.mirror_cluster.to_le_bytes());
    b[0x40] = size_field(RECORD_BYTES as u32, g.cluster_bytes);
    b[0x44] = size_field(g.index_bytes, g.cluster_bytes);
    b[0x48..0x50].copy_from_slice(&g.serial.to_le_bytes());
    // The code runs when a BIOS starts the volume, and it halts at once.
    // 0x54: cli; hlt; jmp to the hlt.
    b[0x54..0x58].copy_from_slice(&[0xFA, 0xF4, 0xEB, 0xFD]);
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(cluster_bytes: u32) -> Geometry {
        Geometry {
            sector_bytes: 512,
            cluster_bytes,
            partition_sectors: 1_000_000,
            hidden_sectors: 2048,
            mft_cluster: 786_432,
            mirror_cluster: 2,
            index_bytes: 4096,
            serial: 0x1234_5678_9ABC_DEF0,
        }
    }

    #[test]
    fn the_boot_sector_has_the_fields_of_ntfs() {
        let b = boot_sector(&geometry(4096));
        assert_eq!(&b[3..11], b"NTFS    ");
        assert_eq!(u16::from_le_bytes([b[0x0B], b[0x0C]]), 512);
        assert_eq!(b[0x0D], 8);
        assert_eq!(b[0x15], 0xF8);
        assert_eq!(&b[0x1C..0x20], &2048u32.to_le_bytes());
        assert_eq!(&b[0x28..0x30], &999_999u64.to_le_bytes());
        assert_eq!(&b[0x30..0x38], &786_432u64.to_le_bytes());
        assert_eq!(&b[0x38..0x40], &2u64.to_le_bytes());
        // A record of 1024 bytes is less than a cluster: 2^10.
        assert_eq!(b[0x40], 0xF6);
        // An index block of 4096 bytes is one cluster.
        assert_eq!(b[0x44], 1);
        assert_eq!(&b[0x48..0x50], &0x1234_5678_9ABC_DEF0u64.to_le_bytes());
        assert_eq!(&b[510..], &[0x55, 0xAA]);
    }

    #[test]
    fn small_clusters_count_records_and_index_blocks_in_clusters() {
        let b = boot_sector(&geometry(512));
        assert_eq!(b[0x0D], 1);
        assert_eq!(b[0x40], 2);
        assert_eq!(b[0x44], 8);
        let b = boot_sector(&geometry(65536));
        assert_eq!(b[0x0D], 128);
        assert_eq!(b[0x44], 0xF4);
    }
}
