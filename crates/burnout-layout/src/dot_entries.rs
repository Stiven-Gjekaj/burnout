//! The first two entries of each directory, in the shape that FAT32 asks
//! for.
//!
//! A FAT32 directory other than the root starts with `.`, which names the
//! directory itself, and `..`, which names the directory above it. `..` names
//! cluster 0 when the directory above is the root. `fatfs` 0.3.6 writes a
//! long-name entry in front of each of the two, and it gives `..` the cluster
//! of the root. `fsck_msdos` reports every directory for it.
//!
//! The source of `fatfs` fixes both, and no release carries the fix. So this
//! reads the volume after `fatfs` has unmounted it, and writes the two
//! entries again where each directory needs them.

use std::io::{self, Read, Seek, SeekFrom, Write};

/// One entry of a directory.
const ENTRY: usize = 32;

/// The attribute bit of a directory.
const DIRECTORY: u8 = 0x10;

/// The attribute bit of the volume label. A long-name entry sets it too.
const VOLUME_ID: u8 = 0x08;

/// The attributes of a long-name entry.
const LONG_NAME: u8 = 0x0F;

/// A first byte that ends the directory.
const END: u8 = 0x00;

/// A first byte that marks an entry as free.
const FREE: u8 = 0xE5;

const DOT: &[u8] = b".          ";
const DOTDOT: &[u8] = b"..         ";

/// A FAT entry at or above this ends a chain.
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;

/// Where the parts of a FAT32 volume are, as its boot sector gives them.
pub(crate) struct Geometry {
    cluster_bytes: u64,
    fat_start: u64,
    data_start: u64,
    root_cluster: u32,
    clusters: u32,
}

impl Geometry {
    pub(crate) fn read<IO: Read + Seek>(io: &mut IO) -> io::Result<Self> {
        let mut boot = [0u8; 512];
        io.seek(SeekFrom::Start(0))?;
        io.read_exact(&mut boot)?;
        let u16_at = |at: usize| u16::from_le_bytes([boot[at], boot[at + 1]]) as u64;
        let u32_at =
            |at: usize| u32::from_le_bytes([boot[at], boot[at + 1], boot[at + 2], boot[at + 3]]);

        let sector = u16_at(11);
        let per_cluster = boot[13] as u64;
        let reserved = u16_at(14);
        let data = reserved + boot[16] as u64 * u32_at(36) as u64;
        let total = u32_at(32) as u64;
        if !(512..=4096).contains(&sector) || per_cluster == 0 || total <= data {
            return Err(bad(
                "the boot sector does not describe a FAT32 volume".to_string()
            ));
        }
        Ok(Geometry {
            cluster_bytes: sector * per_cluster,
            fat_start: reserved * sector,
            data_start: data * sector,
            root_cluster: u32_at(44),
            clusters: ((total - data) / per_cluster) as u32,
        })
    }

    pub(crate) fn root_cluster(&self) -> u32 {
        self.root_cluster
    }

    /// The first byte of a cluster, from the start of the volume.
    pub(crate) fn offset(&self, cluster: u32) -> u64 {
        self.data_start + (cluster as u64 - 2) * self.cluster_bytes
    }

    /// Every cluster of the chain that starts at `first`.
    fn chain<IO: Read + Seek>(&self, io: &mut IO, first: u32) -> io::Result<Vec<u32>> {
        let mut chain = Vec::new();
        let mut cluster = first;
        loop {
            // A chain longer than the volume goes round in a loop.
            let outside = cluster < 2 || cluster - 2 >= self.clusters;
            if outside || chain.len() >= self.clusters as usize {
                return Err(bad(format!(
                    "the chain from cluster {first} runs through {cluster}"
                )));
            }
            chain.push(cluster);
            let mut next = [0u8; 4];
            io.seek(SeekFrom::Start(self.fat_start + 4 * cluster as u64))?;
            io.read_exact(&mut next)?;
            let next = u32::from_le_bytes(next) & 0x0FFF_FFFF;
            if next >= END_OF_CHAIN {
                return Ok(chain);
            }
            cluster = next;
        }
    }
}

/// The first cluster of every directory that `dir` holds, `.` and `..` left
/// out.
pub(crate) fn subdirectories<IO: Read + Seek>(
    io: &mut IO,
    geometry: &Geometry,
    dir: u32,
) -> io::Result<Vec<u32>> {
    let mut found = Vec::new();
    let mut bytes = vec![0u8; geometry.cluster_bytes as usize];
    for cluster in geometry.chain(io, dir)? {
        io.seek(SeekFrom::Start(geometry.offset(cluster)))?;
        io.read_exact(&mut bytes)?;
        for entry in bytes.chunks_exact(ENTRY) {
            match entry[0] {
                END => return Ok(found),
                FREE => continue,
                _ => {}
            }
            let attributes = entry[11];
            let is_dir = attributes & DIRECTORY != 0 && attributes & VOLUME_ID == 0;
            let name = &entry[..11];
            if is_dir && name != DOT && name != DOTDOT {
                found.push(first_cluster(entry));
            }
        }
    }
    Ok(found)
}

/// Write `.` and `..` again at the start of every directory below the root.
///
/// A directory whose entries are in the right place keeps them there, so a
/// second pass changes nothing.
pub(crate) fn repair_dot_entries<IO: Read + Write + Seek>(io: &mut IO) -> io::Result<()> {
    let geometry = Geometry::read(io)?;
    // Each directory still to read, and the cluster that `..` names in each
    // directory it holds. That is 0 below the root.
    let mut pending = vec![(geometry.root_cluster(), 0)];
    while let Some((dir, above)) = pending.pop() {
        for sub in subdirectories(io, &geometry, dir)? {
            repair_one(io, &geometry, sub, above)?;
            pending.push((sub, sub));
        }
    }
    Ok(())
}

/// Put `.` and `..` into the first two entries of the directory at `dir`.
fn repair_one<IO: Read + Write + Seek>(
    io: &mut IO,
    geometry: &Geometry,
    dir: u32,
    above: u32,
) -> io::Result<()> {
    let mut head = [0u8; 4 * ENTRY];
    let at = geometry.offset(dir);
    io.seek(SeekFrom::Start(at))?;
    io.read_exact(&mut head)?;
    let entry = |i: usize| &head[i * ENTRY..(i + 1) * ENTRY];
    let named = |i: usize, name: &[u8]| &entry(i)[..11] == name && entry(i)[11] & DIRECTORY != 0;
    let long = |i: usize| entry(i)[11] == LONG_NAME && entry(i)[0] != FREE;

    let mut fixed = [0u8; 4 * ENTRY];
    if named(0, DOT) && named(1, DOTDOT) {
        fixed.copy_from_slice(&head);
    } else if long(0) && named(1, DOT) && long(2) && named(3, DOTDOT) {
        // The shape that fatfs 0.3.6 writes. The two long-name entries
        // become free entries after `..`.
        fixed[..ENTRY].copy_from_slice(entry(1));
        fixed[ENTRY..2 * ENTRY].copy_from_slice(entry(3));
        fixed[2 * ENTRY] = FREE;
        fixed[3 * ENTRY] = FREE;
    } else {
        return Err(bad(format!(
            "the directory at cluster {dir} does not start with . and .."
        )));
    }
    if first_cluster(&fixed[..ENTRY]) != dir {
        return Err(bad(format!(
            "the . entry of the directory at cluster {dir} names another cluster"
        )));
    }
    set_first_cluster(&mut fixed[ENTRY..2 * ENTRY], above);

    if fixed != head {
        io.seek(SeekFrom::Start(at))?;
        io.write_all(&fixed)?;
    }
    Ok(())
}

pub(crate) fn first_cluster(entry: &[u8]) -> u32 {
    let high = u16::from_le_bytes([entry[20], entry[21]]) as u32;
    let low = u16::from_le_bytes([entry[26], entry[27]]) as u32;
    high << 16 | low
}

fn set_first_cluster(entry: &mut [u8], cluster: u32) {
    entry[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    entry[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
}

fn bad(detail: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fat32::{over_sectors, with_volume};
    use crate::testing::Drive;
    use crate::{copy_to_fat32, MemorySource};
    use burnout_core::{Result, SectorIo};

    fn tree() -> MemorySource {
        let mut tree = MemorySource::new();
        tree.add_file("efi/boot/bootx64.efi", *b"MZ").unwrap();
        tree.add_file("efi/microsoft/boot/BCD", *b"regf").unwrap();
        tree.add_dir("sources").unwrap();
        tree
    }

    /// Every directory below the root: its cluster, the cluster that its `..`
    /// has to name, and its first two entries.
    fn directories<IO: Read + Seek>(io: &mut IO) -> io::Result<Vec<(u32, u32, Vec<u8>)>> {
        let geometry = Geometry::read(io)?;
        let mut found = Vec::new();
        let mut pending = vec![(geometry.root_cluster(), 0)];
        while let Some((dir, above)) = pending.pop() {
            for sub in subdirectories(io, &geometry, dir)? {
                let mut head = vec![0u8; 2 * ENTRY];
                io.seek(SeekFrom::Start(geometry.offset(sub)))?;
                io.read_exact(&mut head)?;
                found.push((sub, above, head));
                pending.push((sub, sub));
            }
        }
        Ok(found)
    }

    fn read_directories(d: &mut Drive) -> Vec<(u32, u32, Vec<u8>)> {
        over_sectors(d.partition(), |io| Ok(directories(io)?)).unwrap()
    }

    #[test]
    fn each_directory_starts_with_dot_and_dotdot_and_no_long_name() {
        for sector in [512, 4096] {
            let mut d = Drive::formatted(sector);
            let manifest = copy_to_fat32(d.partition(), &tree()).unwrap();
            let dirs = read_directories(&mut d);
            // The walk has to find each directory, or it proves nothing.
            assert_eq!(dirs.len(), manifest.dirs.len());
            for (cluster, above, head) in dirs {
                assert_eq!(&head[..11], DOT);
                assert_eq!(head[11], DIRECTORY);
                assert_eq!(first_cluster(&head[..ENTRY]), cluster);
                assert_eq!(&head[ENTRY..ENTRY + 11], DOTDOT);
                assert_eq!(head[ENTRY + 11], DIRECTORY);
                assert_eq!(first_cluster(&head[ENTRY..]), above);
            }
        }
    }

    #[test]
    fn fatfs_alone_still_writes_the_shape_that_this_repairs() {
        // When a release of fatfs fixes this, this test fails, and the
        // repair can go.
        let mut d = Drive::formatted(512);
        with_volume(d.partition(), |fs| {
            fs.root_dir().create_dir("efi")?;
            Ok(())
        })
        .unwrap();
        let dirs = read_directories(&mut d);
        let (_, _, head) = &dirs[0];
        assert_eq!(head[11], LONG_NAME, "a long name comes before .");
        assert_eq!(&head[ENTRY..ENTRY + 11], DOT);
    }

    #[test]
    fn a_second_repair_changes_nothing() {
        let mut d = Drive::formatted(512);
        copy_to_fat32(d.partition(), &tree()).unwrap();
        let before = d.bytes().to_vec();
        over_sectors(d.partition(), |io: &mut SectorIo<_>| -> Result<()> {
            repair_dot_entries(io)?;
            Ok(())
        })
        .unwrap();
        assert!(d.bytes() == before.as_slice());
    }

    #[test]
    fn a_directory_that_does_not_start_with_dot_is_refused() {
        let mut d = Drive::formatted(512);
        copy_to_fat32(d.partition(), &tree()).unwrap();
        let e = over_sectors(d.partition(), |io| -> Result<()> {
            let geometry = Geometry::read(io)?;
            let efi = subdirectories(io, &geometry, geometry.root_cluster())?[0];
            io.seek(SeekFrom::Start(geometry.offset(efi)))?;
            io.write_all(b"NOT A DOT  ")?;
            Ok(repair_dot_entries(io)?)
        })
        .unwrap_err();
        assert!(e.to_string().contains("does not start with"), "{e}");
    }
}
