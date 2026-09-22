//! Where each part of an exFAT volume goes.
//!
//! This is arithmetic and nothing else. It opens no volume and writes no
//! byte, so each rule here is tested with no volume near the test.

use crate::ALIGN;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// The most clusters that a FAT of exFAT can count.
const MAX_CLUSTERS: u64 = (1 << 32) - 11;

/// The first cluster of the cluster heap. Clusters 0 and 1 do not exist, and
/// the FAT keeps two entries for them.
pub(crate) const FIRST_CLUSTER: u32 = 2;

/// Where the parts of an exFAT volume are, in the units of its boot sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Geometry {
    /// The bytes in a sector: 512 or 4096.
    pub sector_bytes: u32,
    /// The sectors in a cluster, which is a power of two.
    pub cluster_sectors: u32,
    /// The sectors in the volume.
    pub volume_sectors: u64,
    /// The first sector of the FAT.
    pub fat_offset: u32,
    /// The sectors of the FAT.
    pub fat_length: u32,
    /// The first sector of the cluster heap.
    pub heap_offset: u32,
    /// The clusters in the cluster heap.
    pub cluster_count: u32,
}

impl Geometry {
    /// The geometry of a volume of `volume_bytes` on sectors of
    /// `sector_bytes`.
    ///
    /// The FAT starts 1 MiB into the volume, and the cluster heap starts on
    /// the first whole mebibyte after the FAT. `None` when the volume has no
    /// room for one cluster after that.
    pub(crate) fn new(volume_bytes: u64, sector_bytes: u32) -> Option<Self> {
        let sector = sector_bytes as u64;
        let volume_sectors = volume_bytes / sector;
        let cluster_sectors = cluster_bytes(volume_bytes, sector_bytes) / sector;
        let align = ALIGN / sector;

        let fat_offset = align;
        // A FAT for every cluster that the volume could hold after it. The
        // heap then takes some of that room, so the true count is lower and
        // this FAT is large enough for it.
        let most = volume_sectors.checked_sub(fat_offset)? / cluster_sectors;
        let heap_offset = (fat_offset + fat_sectors(most, sector)).div_ceil(align) * align;
        let cluster_count =
            (volume_sectors.checked_sub(heap_offset)? / cluster_sectors).min(MAX_CLUSTERS);
        if cluster_count == 0 {
            return None;
        }
        Some(Geometry {
            sector_bytes,
            cluster_sectors: cluster_sectors as u32,
            volume_sectors,
            fat_offset: u32::try_from(fat_offset).ok()?,
            fat_length: u32::try_from(fat_sectors(cluster_count, sector)).ok()?,
            heap_offset: u32::try_from(heap_offset).ok()?,
            cluster_count: cluster_count as u32,
        })
    }

    /// The bytes in a cluster.
    pub(crate) fn cluster_bytes(&self) -> u64 {
        self.cluster_sectors as u64 * self.sector_bytes as u64
    }

    /// How many clusters hold `bytes` bytes.
    pub(crate) fn clusters_for(&self, bytes: u64) -> u64 {
        bytes.div_ceil(self.cluster_bytes())
    }

    /// The byte of the volume where cluster `cluster` starts.
    pub(crate) fn cluster_offset(&self, cluster: u32) -> u64 {
        self.heap_offset as u64 * self.sector_bytes as u64
            + (cluster - FIRST_CLUSTER) as u64 * self.cluster_bytes()
    }

    /// The byte of the volume where the FAT starts.
    pub(crate) fn fat_start(&self) -> u64 {
        self.fat_offset as u64 * self.sector_bytes as u64
    }

    /// The bytes of the allocation bitmap: one bit for each cluster.
    pub(crate) fn bitmap_bytes(&self) -> u64 {
        (self.cluster_count as u64).div_ceil(8)
    }

    /// The size of a sector as the boot sector gives it, as a power of two.
    pub(crate) fn sector_shift(&self) -> u8 {
        self.sector_bytes.trailing_zeros() as u8
    }

    /// The size of a cluster as the boot sector gives it, in sectors, as a
    /// power of two.
    pub(crate) fn cluster_shift(&self) -> u8 {
        self.cluster_sectors.trailing_zeros() as u8
    }
}

/// The cluster size that Windows picks for an exFAT volume of this size, and
/// never less than a sector.
fn cluster_bytes(volume_bytes: u64, sector_bytes: u32) -> u64 {
    let bytes = if volume_bytes <= 256 * MIB {
        4 * KIB
    } else if volume_bytes <= 32 * GIB {
        32 * KIB
    } else {
        128 * KIB
    };
    bytes.max(sector_bytes as u64)
}

/// The sectors of a FAT for `clusters` clusters: four bytes for each, and
/// for the two entries before the first.
fn fat_sectors(clusters: u64, sector: u64) -> u64 {
    ((clusters + 2) * 4).div_ceil(sector)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [u64; 7] = [
        4 * MIB,
        64 * MIB,
        256 * MIB,
        300 * MIB,
        6 * GIB + 200 * MIB,
        128_300_000_000 / MIB * MIB,
        2048 * GIB - MIB,
    ];

    fn geometries() -> Vec<Geometry> {
        let mut all = Vec::new();
        for sector in [512u32, 4096] {
            for size in SIZES {
                all.push(Geometry::new(size, sector).unwrap());
            }
        }
        all
    }

    #[test]
    fn the_cluster_is_the_one_that_windows_picks() {
        let cluster = |size| Geometry::new(size, 512).unwrap().cluster_bytes();
        assert_eq!(cluster(64 * MIB), 4 * KIB);
        assert_eq!(cluster(256 * MIB), 4 * KIB);
        assert_eq!(cluster(257 * MIB), 32 * KIB);
        assert_eq!(cluster(32 * GIB), 32 * KIB);
        assert_eq!(cluster(32 * GIB + MIB), 128 * KIB);
    }

    #[test]
    fn a_cluster_is_never_smaller_than_a_sector() {
        let g = Geometry::new(64 * MIB, 4096).unwrap();
        assert_eq!(g.cluster_bytes(), 4096);
        assert_eq!(g.cluster_sectors, 1);
        assert_eq!(g.cluster_shift(), 0);
        assert_eq!(g.sector_shift(), 12);
    }

    #[test]
    fn the_fat_starts_at_one_mebibyte_and_the_heap_on_a_later_one() {
        for g in geometries() {
            assert_eq!(g.fat_start(), MIB, "{g:?}");
            let heap = g.heap_offset as u64 * g.sector_bytes as u64;
            assert_eq!(heap % MIB, 0, "{g:?}");
            assert!(g.fat_offset + g.fat_length <= g.heap_offset, "{g:?}");
        }
    }

    #[test]
    fn the_fat_holds_an_entry_for_each_cluster_and_no_spare_sector() {
        for g in geometries() {
            let entries = (g.cluster_count as u64 + 2) * 4;
            let fat = g.fat_length as u64 * g.sector_bytes as u64;
            assert!(fat >= entries, "{g:?}");
            assert!(fat - entries < g.sector_bytes as u64, "{g:?}");
        }
    }

    #[test]
    fn the_clusters_fill_the_volume_after_the_heap_starts() {
        for g in geometries() {
            let end = g.heap_offset as u64 + g.cluster_count as u64 * g.cluster_sectors as u64;
            assert!(end <= g.volume_sectors, "{g:?}");
            assert!(end + g.cluster_sectors as u64 > g.volume_sectors, "{g:?}");
        }
    }

    #[test]
    fn a_cluster_starts_where_the_one_before_it_ends() {
        let g = Geometry::new(64 * MIB, 512).unwrap();
        assert_eq!(g.cluster_offset(2), 2 * MIB);
        assert_eq!(g.cluster_offset(3), 2 * MIB + 4 * KIB);
        // A cluster past 4 GiB into the volume needs 64 bits.
        let g = Geometry::new(6 * GIB + 200 * MIB, 512).unwrap();
        assert_eq!(g.cluster_offset(200_000), 2 * MIB + 199_998 * 32 * KIB);
    }

    #[test]
    fn the_largest_volume_that_the_table_names_counts_its_clusters_in_32_bits() {
        // 2 TiB is the end of an MBR on 512-byte sectors, and 16 TiB on
        // 4096-byte sectors.
        let g = Geometry::new(2048 * GIB, 512).unwrap();
        assert_eq!(g.cluster_bytes(), 128 * KIB);
        assert!(g.cluster_count as u64 <= MAX_CLUSTERS);
        // There the FAT alone takes 512 MiB, so the heap starts at 513 MiB.
        let g = Geometry::new(16 * 1024 * GIB, 4096).unwrap();
        assert_eq!(g.heap_offset as u64 * 4096, 513 * MIB);
        assert_eq!(
            g.cluster_count as u64,
            (16 * 1024 * GIB - 513 * MIB) / (128 * KIB)
        );
    }

    #[test]
    fn the_bitmap_has_a_bit_for_each_cluster() {
        let g = Geometry::new(64 * MIB, 512).unwrap();
        assert_eq!(g.bitmap_bytes(), (g.cluster_count as u64).div_ceil(8));
        assert_eq!(g.clusters_for(0), 0);
        assert_eq!(g.clusters_for(1), 1);
        assert_eq!(g.clusters_for(4 * KIB), 1);
        assert_eq!(g.clusters_for(4 * KIB + 1), 2);
    }

    #[test]
    fn a_volume_with_no_room_for_a_cluster_has_no_geometry() {
        for sector in [512u32, 4096] {
            assert_eq!(Geometry::new(MIB, sector), None);
            assert_eq!(Geometry::new(2 * MIB, sector), None);
            let g = Geometry::new(2 * MIB + 4 * KIB, sector).unwrap();
            assert_eq!(g.cluster_count, 1);
        }
    }
}
