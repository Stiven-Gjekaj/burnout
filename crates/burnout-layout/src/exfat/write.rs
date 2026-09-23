//! Write an exFAT volume with a tree on it, in one pass.

use std::io::{self, SeekFrom};

use burnout_core::{BlockTarget, Error, Result};

use super::boot::{boot_region, BootFields, REGION_SECTORS};
use super::entries::{label_entry, Kind};
use super::geometry::{Geometry, FIRST_CLUSTER};
use super::place::{min_volume_bytes, Placement, Run};
use super::upcase::{recommended_bytes, UpCase};
use crate::manifest::{CopiedFile, Manifest};
use crate::stream::{stream_file, CHUNK_BYTES};
use burnout_core::FileSource;

/// The entry of the FAT for the first of the two clusters that do not exist:
/// the media type F8h, as on a fixed disk, and ones.
const MEDIA: u32 = 0xFFFF_FFF8;

/// The entry of the FAT that ends a chain.
const END_OF_CHAIN: u32 = 0xFFFF_FFFF;

/// What a new exFAT volume is called, and where it sits on the drive.
#[derive(Clone, Copy, Debug)]
pub struct ExfatOptions<'a> {
    /// The name that a host shows for the volume.
    ///
    /// exFAT keeps 11 UTF-16 units of it, in the case it has. A character
    /// that a name cannot hold becomes `_`. An empty label gives a volume
    /// with no label.
    pub label: &'a str,
    /// The volume serial number.
    ///
    /// Windows shows it and tells volumes apart by it, so the caller gives a
    /// number of its own for each drive.
    pub serial: u32,
    /// The sector of the drive where the partition starts, as the partition
    /// table counts it.
    pub first_sector: u64,
}

/// Format the whole of `volume` as exFAT, and copy every directory and every
/// file of `source` onto it.
///
/// `volume` is a partition in the product, and anything that takes whole
/// sectors in a test. Every access to it is whole sectors.
///
/// The whole tree is placed before a byte is written. A tree that does not
/// fit, or a name that exFAT cannot hold, leaves the volume as it was. The
/// boot regions go last, so a write that stops half way leaves nothing that a
/// host mounts.
///
/// The digest of each file is of the bytes that came from the source, taken
/// on their way onto the volume.
pub fn write_exfat<T, S>(mut volume: T, source: &S, options: &ExfatOptions) -> Result<Manifest>
where
    T: BlockTarget,
    S: FileSource + ?Sized,
{
    let sector = volume.logical_sector_size();
    let volume_bytes = volume.length();
    let up = UpCase::recommended();
    let label = label_entry(options.label);

    let too_small = || Error::PartitionTooSmall {
        file_system: "exFAT",
        partition_bytes: volume_bytes,
        needed_bytes: min_volume_bytes(sector),
    };
    let geometry = Geometry::new(volume_bytes, sector).ok_or_else(too_small)?;
    Placement::new(geometry, label, &[], &up).map_err(|_| too_small())?;
    let placement = Placement::new(geometry, label, &source.entries()?, &up)?;

    write_fat(&mut volume, &placement)?;
    write_run(
        &mut volume,
        &geometry,
        placement.bitmap,
        &bitmap(&placement),
    )?;
    write_run(
        &mut volume,
        &geometry,
        placement.upcase,
        &recommended_bytes(),
    )?;
    write_run(
        &mut volume,
        &geometry,
        placement.root,
        &placement.root_directory(),
    )?;
    let manifest = write_items(&mut volume, &placement, source)?;
    volume.flush()?;

    let region = boot_region(
        &geometry,
        &BootFields {
            partition_offset: options.first_sector,
            serial: options.serial,
            root_cluster: placement.root.first,
            percent_in_use: placement.percent_in_use(),
        },
    );
    // The copy first and the boot region itself last, so a volume that has
    // a boot sector at all has both.
    for at in [REGION_SECTORS, 0] {
        volume.seek(SeekFrom::Start(at * sector as u64))?;
        volume.write_all(&region)?;
    }
    volume.flush()?;
    Ok(manifest)
}

/// Write the FAT: the two entries before the first cluster, a chain for the
/// bitmap, the up-case table and the root directory, and zero for the rest.
///
/// A file or a directory of the tree is one run that its entry records, so
/// the FAT holds no chain for it.
fn write_fat<T: BlockTarget>(volume: &mut T, placement: &Placement) -> io::Result<()> {
    let g = &placement.geometry;
    let sector = g.sector_bytes as usize;
    let mut entries = vec![0u32; placement.root.end() as usize];
    entries[0] = MEDIA;
    entries[1] = END_OF_CHAIN;
    for run in [placement.bitmap, placement.upcase, placement.root] {
        for cluster in run.first..run.end() {
            entries[cluster as usize] = if cluster + 1 == run.end() {
                END_OF_CHAIN
            } else {
                cluster + 1
            };
        }
    }
    let mut head: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
    head.resize(head.len().div_ceil(sector) * sector, 0);

    volume.seek(SeekFrom::Start(g.fat_start()))?;
    volume.write_all(&head)?;
    let mut rest = g.fat_length as u64 * sector as u64 - head.len() as u64;
    let zeros = vec![0u8; CHUNK_BYTES];
    while rest > 0 {
        let n = rest.min(CHUNK_BYTES as u64) as usize;
        volume.write_all(&zeros[..n])?;
        rest -= n as u64;
    }
    Ok(())
}

/// The allocation bitmap: every cluster from the first to the last run is in
/// use, and no other.
fn bitmap(placement: &Placement) -> Vec<u8> {
    let used = placement.used_clusters() as usize;
    let mut bits = vec![0u8; placement.geometry.bitmap_bytes() as usize];
    bits[..used / 8].fill(0xFF);
    if used % 8 != 0 {
        bits[used / 8] = (1u8 << (used % 8)) - 1;
    }
    bits
}

/// Write `bytes` into a run, and zero to the end of its last cluster.
fn write_run<T: BlockTarget>(
    volume: &mut T,
    g: &Geometry,
    run: Run,
    bytes: &[u8],
) -> io::Result<()> {
    let length = (run.clusters as u64 * g.cluster_bytes()) as usize;
    debug_assert!(bytes.len() <= length);
    let mut padded = bytes.to_vec();
    padded.resize(length, 0);
    volume.seek(SeekFrom::Start(g.cluster_offset(run.first)))?;
    volume.write_all(&padded)
}

/// Write each directory and each file of the tree into its run, in the
/// order of the tree.
fn write_items<T, S>(volume: &mut T, placement: &Placement, source: &S) -> Result<Manifest>
where
    T: BlockTarget,
    S: FileSource + ?Sized,
{
    let g = &placement.geometry;
    let cluster = g.cluster_bytes() as usize;
    let mut manifest = Manifest::default();
    let mut chunk = vec![0u8; CHUNK_BYTES];
    let mut tail = Vec::with_capacity(CHUNK_BYTES);
    for item in &placement.items {
        match item.kind {
            Kind::Dir => {
                write_run(volume, g, item.run, &placement.directory(item))?;
                manifest.dirs.push(item.path.clone());
            }
            Kind::File => {
                if item.run.first >= FIRST_CLUSTER {
                    volume.seek(SeekFrom::Start(g.cluster_offset(item.run.first)))?;
                }
                let mut reader = source.open(&item.path)?;
                let digest = stream_file(
                    &mut *reader,
                    &item.path,
                    item.data_length,
                    &mut chunk,
                    |piece| {
                        // A whole chunk is whole clusters. Only the last
                        // piece can end inside a cluster, and the rest of that
                        // cluster goes to zero.
                        if piece.len() == CHUNK_BYTES {
                            return volume.write_all(piece);
                        }
                        tail.clear();
                        tail.extend_from_slice(piece);
                        tail.resize(piece.len().div_ceil(cluster) * cluster, 0);
                        volume.write_all(&tail)
                    },
                )?;
                manifest.files.push(CopiedFile {
                    path: item.path.clone(),
                    bytes: item.data_length,
                    digest,
                });
            }
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{pattern, windows_like, Drive, MIB};
    use burnout_core::{Entry, MemorySource, TreePath};
    use std::io::{Read, Write};

    const SERIAL: u32 = 0x1234_ABCD;

    fn options() -> ExfatOptions<'static> {
        ExfatOptions {
            label: "Install",
            serial: SERIAL,
            first_sector: 0,
        }
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// Where the tree went, from the same plan that the writer made.
    fn placement(d: &Drive, tree: &MemorySource) -> Placement {
        let g = Geometry::new(d.length, d.sector()).unwrap();
        Placement::new(
            g,
            label_entry("Install"),
            &tree.entries().unwrap(),
            &UpCase::recommended(),
        )
        .unwrap()
    }

    #[test]
    fn each_file_goes_into_its_run_with_the_digest_of_its_bytes() {
        let tree = windows_like();
        for sector in [512, 4096] {
            let mut d = Drive::new(sector, 64 * MIB);
            let manifest = write_exfat(d.partition(), &tree, &options()).unwrap();
            let p = placement(&d, &tree);
            let volume = d.partition_bytes();
            assert_eq!(manifest.files.len(), 6);
            for (copied, item) in manifest
                .files
                .iter()
                .zip(p.items.iter().filter(|i| i.kind == Kind::File))
            {
                let mut want = Vec::new();
                tree.open(&copied.path)
                    .unwrap()
                    .read_to_end(&mut want)
                    .unwrap();
                assert_eq!(copied.path, item.path);
                assert_eq!(copied.digest, burnout_core::sha256(&want));
                let at = p.geometry.cluster_offset(item.run.first.max(2)) as usize;
                assert_eq!(
                    &volume[at..at + want.len()],
                    want.as_slice(),
                    "{}",
                    item.path
                );
            }
        }
    }

    #[test]
    fn every_directory_goes_into_the_manifest_in_the_order_of_the_tree() {
        let mut d = Drive::new(512, 64 * MIB);
        let manifest = write_exfat(d.partition(), &windows_like(), &options()).unwrap();
        let dirs: Vec<&str> = manifest.dirs.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            dirs,
            [
                "efi",
                "efi/boot",
                "efi/microsoft",
                "efi/microsoft/boot",
                "sources",
                "support",
                "support/logging"
            ]
        );
    }

    #[test]
    fn the_boot_region_and_its_copy_go_at_sectors_0_and_12() {
        for sector in [512usize, 4096] {
            let mut d = Drive::new(sector as u32, 64 * MIB);
            write_exfat(d.partition(), &windows_like(), &options()).unwrap();
            let volume = d.partition_bytes();
            assert_eq!(&volume[3..11], b"EXFAT   ");
            assert_eq!(u32_at(volume, 100), SERIAL);
            assert_eq!(&volume[..12 * sector], &volume[12 * sector..24 * sector]);
        }
    }

    #[test]
    fn the_fat_chains_the_bitmap_the_up_case_table_and_the_root_and_nothing_else() {
        let tree = windows_like();
        let mut d = Drive::new(512, 64 * MIB);
        write_exfat(d.partition(), &tree, &options()).unwrap();
        let p = placement(&d, &tree);
        let volume = d.partition_bytes();
        let fat = p.geometry.fat_start() as usize;
        let entry = |cluster: u32| u32_at(volume, fat + 4 * cluster as usize);
        assert_eq!(entry(0), 0xFFFF_FFF8);
        assert_eq!(entry(1), 0xFFFF_FFFF);
        // The bitmap is one cluster, the up-case table two, the root one.
        assert_eq!(
            (p.bitmap.clusters, p.upcase.clusters, p.root.clusters),
            (1, 2, 1)
        );
        assert_eq!(entry(2), 0xFFFF_FFFF);
        assert_eq!(entry(3), 4);
        assert_eq!(entry(4), 0xFFFF_FFFF);
        assert_eq!(entry(5), 0xFFFF_FFFF);
        let rest = fat + 4 * 6..fat + p.geometry.fat_length as usize * 512;
        assert!(volume[rest].iter().all(|b| *b == 0));
    }

    #[test]
    fn the_bitmap_marks_every_cluster_in_use_and_no_other() {
        let tree = windows_like();
        let mut d = Drive::new(512, 64 * MIB);
        write_exfat(d.partition(), &tree, &options()).unwrap();
        let p = placement(&d, &tree);
        let volume = d.partition_bytes();
        let start = p.geometry.cluster_offset(2) as usize;
        let bits = &volume[start..start + p.geometry.bitmap_bytes() as usize];
        let set: u64 = bits.iter().map(|b| b.count_ones() as u64).sum();
        assert_eq!(set, p.used_clusters());
        let used = p.used_clusters() as usize;
        assert_eq!(bits[(used - 1) / 8] >> ((used - 1) % 8) & 1, 1);
        assert_eq!(bits[used / 8] >> (used % 8) & 1, 0);
        assert_eq!(volume[112], p.percent_in_use());
    }

    #[test]
    fn the_rest_of_the_last_cluster_of_a_file_is_zero() {
        // The drive holds old data, as a used stick does.
        let mut tree = MemorySource::new();
        tree.add_file("a.bin", pattern(5000, 9)).unwrap();
        let mut d = Drive::new(512, 64 * MIB);
        let size = d.bytes().len();
        d.strict.write_all(&vec![0xAA; size]).unwrap();
        write_exfat(d.partition(), &tree, &options()).unwrap();
        let p = placement(&d, &tree);
        let at = p.geometry.cluster_offset(p.items[0].run.first) as usize;
        let volume = d.partition_bytes();
        assert_eq!(&volume[at..at + 5000], pattern(5000, 9).as_slice());
        assert!(volume[at + 5000..at + 8192].iter().all(|b| *b == 0));
    }

    #[test]
    fn the_same_tree_makes_the_same_volume() {
        let mut first = Drive::new(512, 64 * MIB);
        let mut second = Drive::new(512, 64 * MIB);
        write_exfat(first.partition(), &windows_like(), &options()).unwrap();
        write_exfat(second.partition(), &windows_like(), &options()).unwrap();
        assert!(first.bytes() == second.bytes());
    }

    #[test]
    fn nothing_outside_the_partition_changes() {
        let mut d = Drive::new(512, 64 * MIB);
        let size = d.bytes().len();
        d.strict.write_all(&vec![0xAA; size]).unwrap();
        write_exfat(d.partition(), &windows_like(), &options()).unwrap();
        let bytes = d.bytes();
        assert!(bytes[..MIB as usize].iter().all(|b| *b == 0xAA));
        assert!(bytes[(65 * MIB) as usize..].iter().all(|b| *b == 0xAA));
    }

    #[test]
    fn a_tree_that_does_not_fit_leaves_the_volume_as_it_was() {
        let mut tree = MemorySource::new();
        tree.add_file("big.bin", vec![1u8; 9 * MIB as usize])
            .unwrap();
        let mut d = Drive::new(512, 8 * MIB);
        let before = d.bytes().to_vec();
        let e = write_exfat(d.partition(), &tree, &options()).unwrap_err();
        assert!(matches!(e, Error::VolumeFull { .. }), "{e}");
        assert!(d.bytes() == before.as_slice());
    }

    #[test]
    fn a_partition_too_small_for_exfat_is_refused_with_the_smallest_that_fits() {
        for sector in [512u32, 4096] {
            let smallest = min_volume_bytes(sector);
            let mut d = Drive::new(sector, smallest);
            write_exfat(d.partition(), &MemorySource::new(), &options()).unwrap();

            let mut d = Drive::new(sector, smallest - sector as u64);
            match write_exfat(d.partition(), &MemorySource::new(), &options()) {
                Err(Error::PartitionTooSmall { needed_bytes, .. }) => {
                    assert_eq!(needed_bytes, smallest)
                }
                other => panic!("{sector}: {other:?}"),
            }
        }
    }

    /// A tree of one file that says it holds 10 bytes and gives 20.
    struct Liar;

    impl FileSource for Liar {
        fn entries(&self) -> Result<Vec<Entry>> {
            Ok(vec![Entry::File(TreePath::new("install.wim")?, 10)])
        }

        fn open(&self, _path: &TreePath) -> Result<Box<dyn Read + '_>> {
            Ok(Box::new(io::repeat(0x5A).take(20)))
        }
    }

    #[test]
    fn a_copy_that_fails_half_way_leaves_nothing_that_mounts() {
        let mut d = Drive::new(512, 64 * MIB);
        let e = write_exfat(d.partition(), &Liar, &options()).unwrap_err();
        assert!(matches!(e, Error::Source { .. }), "{e}");
        let volume = d.partition_bytes();
        assert!(volume[..24 * 512].iter().all(|b| *b == 0), "no boot region");
    }
}
