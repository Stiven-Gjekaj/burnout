//! Read an exFAT volume back, with no help from the writer.
//!
//! The check after a copy reads the volume through this. It takes each
//! number from the volume itself, and it keeps no cache between two reads of
//! the drive, so it reads what the volume holds. It shares only the checksums
//! and the up-case table of the specification with the writer.
//!
//! It is strict. It refuses a volume that the specification calls invalid,
//! and also a volume that is valid but that Burnout never writes, such as one
//! with two FATs.

use std::collections::HashSet;
use std::io::{self, SeekFrom};

use burnout_core::{BlockTarget, Error, Result};

use super::sums::{boot_checksum, name_hash, set_checksum, table_checksum};
use super::upcase::UpCase;
use crate::tree::TreePath;

/// The bytes of one directory entry.
const ENTRY: usize = 32;

/// The FAT entry that ends a chain.
const END_OF_CHAIN: u32 = 0xFFFF_FFFF;

/// How much of a file one read takes.
const CHUNK_BYTES: u64 = 1024 * 1024;

/// The largest directory of exFAT.
const MAX_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;

/// Where the data of an entry is, as its stream extension records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Data {
    pub first: u32,
    pub length: u64,
    /// The bytes of the data that were written. The rest reads as zero.
    pub valid: u64,
    /// One run of clusters that the FAT does not record.
    pub contiguous: bool,
}

/// A directory or a file that the volume holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub path: TreePath,
    pub is_dir: bool,
    pub data: Data,
}

/// A run of clusters: the first, and how many.
type Run = (u32, u32);

/// An exFAT volume, open for reading.
pub(crate) struct Volume<T: BlockTarget> {
    target: T,
    sector: u64,
    cluster: u64,
    fat_start: u64,
    heap_start: u64,
    count: u32,
    percent: u8,
    up: UpCase,
    bitmap: Vec<u8>,
    root: Vec<u8>,
    /// A bit for each cluster that an allocation claims.
    claimed: Vec<u8>,
}

impl<T: BlockTarget> Volume<T> {
    /// Open the exFAT volume that fills `target`, and check its boot region,
    /// its FAT, its root directory, its up-case table and its bitmap.
    pub(crate) fn open(mut target: T) -> Result<Self> {
        let sector = target.logical_sector_size() as u64;
        if target.length() < 24 * sector {
            return Err(invalid("the volume is smaller than its two boot regions"));
        }
        let mut regions = vec![0u8; 24 * sector as usize];
        target.seek(SeekFrom::Start(0))?;
        target.read_exact(&mut regions)?;
        let (main, backup) = regions.split_at(12 * sector as usize);
        check_region(main, sector as usize, "the boot region")?;
        check_region(backup, sector as usize, "the copy of the boot region")?;
        for at in 0..main.len() {
            if main[at] != backup[at] && !matches!(at, 106 | 107 | 112) {
                return Err(invalid(format!(
                    "the copy of the boot region differs from it at byte {at}"
                )));
            }
        }

        let boot = &main[..sector as usize];
        let u32_at = |at: usize| u32::from_le_bytes(boot[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(boot[at..at + 8].try_into().unwrap());
        let volume_sectors = u64_at(72);
        let fat_offset = u32_at(80) as u64;
        let fat_length = u32_at(84) as u64;
        let heap_offset = u32_at(88) as u64;
        let count = u32_at(92);
        let root_cluster = u32_at(96);
        let revision = u16::from_le_bytes([boot[104], boot[105]]);
        let flags = u16::from_le_bytes([boot[106], boot[107]]);
        let (sector_shift, cluster_shift, fats, percent) =
            (boot[108], boot[109], boot[110], boot[112]);

        if 1u64.checked_shl(sector_shift as u32) != Some(sector) {
            return Err(invalid(format!(
                "its sectors are 2^{sector_shift} bytes, and the drive has sectors of {sector}"
            )));
        }
        if cluster_shift as u32 > 25 - sector_shift as u32 {
            return Err(invalid("its clusters are larger than 32 MiB"));
        }
        let cluster = sector << cluster_shift;
        if revision >> 8 != 1 {
            return Err(invalid(format!("its revision is {revision:#06x}, not 1.x")));
        }
        if fats != 1 {
            return Err(invalid(format!(
                "it has {fats} FATs, and Burnout writes one"
            )));
        }
        if flags & 0b1111 != 0 {
            return Err(invalid(format!(
                "its volume flags are {flags:#06x}, not clean"
            )));
        }
        if volume_sectors * sector > target.length() {
            return Err(invalid("it is larger than the partition that holds it"));
        }
        let heap_end = heap_offset + count as u64 * (cluster / sector);
        if fat_offset < 24
            || fat_offset + fat_length > heap_offset
            || fat_length * sector < (count as u64 + 2) * 4
            || heap_end > volume_sectors
            || count == 0
        {
            return Err(invalid(
                "its FAT and its cluster heap do not fit where its boot sector puts them",
            ));
        }
        if root_cluster < 2 || root_cluster > count + 1 {
            return Err(invalid(format!(
                "its root directory starts at cluster {root_cluster}"
            )));
        }

        let bitmap_bytes = (count as usize).div_ceil(8);
        let mut volume = Volume {
            target,
            sector,
            cluster,
            fat_start: fat_offset * sector,
            heap_start: heap_offset * sector,
            count,
            percent,
            up: UpCase::recommended(),
            bitmap: Vec::new(),
            root: Vec::new(),
            claimed: vec![0u8; bitmap_bytes],
        };

        let media = volume.fat_entry(0)?;
        let one = volume.fat_entry(1)?;
        if media >> 8 != 0x00FF_FFFF || one != END_OF_CHAIN {
            return Err(invalid(format!(
                "its FAT starts with {media:#010x} and {one:#010x}"
            )));
        }

        let root_runs = volume.chain(root_cluster, None)?;
        volume.claim(&root_runs, "the root directory")?;
        let root_bytes = runs_clusters(&root_runs) * cluster;
        if root_bytes > MAX_DIRECTORY_BYTES {
            return Err(invalid("its root directory is larger than 256 MiB"));
        }
        volume.root = volume.read_runs(&root_runs, root_bytes)?;
        volume.read_system_entries(bitmap_bytes as u64)?;
        Ok(volume)
    }

    /// Read the entries of the root that describe the volume itself: the
    /// bitmap, the up-case table and the label.
    fn read_system_entries(&mut self, bitmap_bytes: u64) -> Result<()> {
        let mut bitmap = None;
        let mut upcase = None;
        let mut seen_label = false;
        for entry in self.root.chunks_exact(ENTRY) {
            match entry[0] {
                0x00 => break,
                0x81 => {
                    if bitmap.replace(stream_of(entry)).is_some() || entry[1] & 1 != 0 {
                        return Err(invalid("its root holds more than the one bitmap it can"));
                    }
                }
                0x82 => {
                    let checksum = u32::from_le_bytes(entry[4..8].try_into().unwrap());
                    if upcase.replace((stream_of(entry), checksum)).is_some() {
                        return Err(invalid("its root holds two up-case tables"));
                    }
                }
                0x83 => {
                    let length = entry[1] as usize;
                    if length > 11 || seen_label {
                        return Err(invalid("its label is not one label of 11 units or fewer"));
                    }
                    let units: Vec<u16> = entry[2..2 + 2 * length]
                        .chunks_exact(2)
                        .map(|p| u16::from_le_bytes([p[0], p[1]]))
                        .collect();
                    String::from_utf16(&units)
                        .map_err(|_| invalid("its label is not valid UTF-16"))?;
                    seen_label = true;
                }
                _ => {}
            }
        }

        let (first, length) = bitmap.ok_or_else(|| invalid("its root holds no bitmap"))?;
        if length != bitmap_bytes {
            return Err(invalid(format!(
                "its bitmap holds {length} bytes, and {} clusters need {bitmap_bytes}",
                self.count
            )));
        }
        let runs = self.chain(first, Some(length))?;
        self.claim(&runs, "the bitmap")?;
        self.bitmap = self.read_runs(&runs, length)?;

        let ((first, length), checksum) =
            upcase.ok_or_else(|| invalid("its root holds no up-case table"))?;
        let runs = self.chain(first, Some(length))?;
        self.claim(&runs, "the up-case table")?;
        let table = self.read_runs(&runs, length)?;
        if table_checksum(&table) != checksum {
            return Err(invalid(
                "its up-case table does not have the checksum its entry gives",
            ));
        }
        self.up = UpCase::from_bytes(&table)
            .ok_or_else(|| invalid("its up-case table is not a table"))?;
        Ok(())
    }

    /// Every directory and every file of the volume, a directory before
    /// what it holds.
    pub(crate) fn walk(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        let root = std::mem::take(&mut self.root);
        let result = self.walk_dir(&root, None, &mut nodes);
        self.root = root;
        result.map(|()| nodes)
    }

    fn walk_dir(
        &mut self,
        bytes: &[u8],
        above: Option<&TreePath>,
        nodes: &mut Vec<Node>,
    ) -> Result<()> {
        let at_dir = || above.map_or_else(|| "the root directory".to_string(), |p| p.to_string());
        let mut names = HashSet::new();
        let mut at = 0;
        let mut ended = false;
        while at + ENTRY <= bytes.len() {
            let kind = bytes[at];
            if ended {
                if kind != 0x00 {
                    return Err(differs(
                        &at_dir(),
                        "an entry follows the end of the directory",
                    ));
                }
                at += ENTRY;
                continue;
            }
            match kind {
                0x00 => {
                    ended = true;
                    at += ENTRY;
                }
                // An entry that is not in use.
                0x01..=0x7F => at += ENTRY,
                // The bitmap, the up-case table and the label belong to the
                // root, and the root was read for them.
                0x81..=0x83 if above.is_none() => at += ENTRY,
                0x85 => {
                    let secondary = bytes[at + 1] as usize;
                    let end = at + (1 + secondary) * ENTRY;
                    if secondary < 2 || end > bytes.len() {
                        return Err(differs(&at_dir(), "an entry set runs past its directory"));
                    }
                    let node = self.read_set(&bytes[at..end], above, &mut names)?;
                    at = end;
                    let is_dir = node.is_dir;
                    let data = node.data;
                    let path = node.path.clone();
                    nodes.push(node);
                    if is_dir {
                        if data.length > MAX_DIRECTORY_BYTES {
                            return Err(differs(
                                path.as_str(),
                                "the directory is larger than 256 MiB",
                            ));
                        }
                        let runs = self.runs_of(&data, path.as_str())?;
                        let inner = self.read_runs(&runs, data.length)?;
                        self.walk_dir(&inner, Some(&path), nodes)?;
                    }
                }
                other => {
                    return Err(differs(
                        &at_dir(),
                        &format!("the directory holds an entry of type {other:#04x}"),
                    ))
                }
            }
        }
        Ok(())
    }

    /// Read one entry set of a file or a directory, and claim its clusters.
    fn read_set(
        &mut self,
        set: &[u8],
        above: Option<&TreePath>,
        names: &mut HashSet<Vec<u16>>,
    ) -> Result<Node> {
        let at_dir = above.map_or("the root directory", |p| p.as_str());
        let stored = u16::from_le_bytes([set[2], set[3]]);
        if set_checksum(set) != stored {
            return Err(differs(
                at_dir,
                "an entry set does not have the checksum it records",
            ));
        }
        let stream = &set[ENTRY..2 * ENTRY];
        if stream[0] != 0xC0 || stream[1] & 1 == 0 {
            return Err(differs(
                at_dir,
                "an entry set has no stream extension after its file entry",
            ));
        }
        let name_length = stream[3] as usize;
        let name_entries = name_length.div_ceil(15);
        if name_length == 0 || set.len() < (2 + name_entries) * ENTRY {
            return Err(differs(
                at_dir,
                "an entry set has too few name entries for its name",
            ));
        }
        let mut units = Vec::with_capacity(name_length);
        for entry in set[2 * ENTRY..].chunks_exact(ENTRY).take(name_entries) {
            if entry[0] != 0xC1 {
                return Err(differs(
                    at_dir,
                    "an entry set has too few name entries for its name",
                ));
            }
            units.extend(
                entry[2..]
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]])),
            );
        }
        units.truncate(name_length);
        for entry in set[(2 + name_entries) * ENTRY..].chunks_exact(ENTRY) {
            // A benign secondary entry can follow, and a reader may skip it.
            if entry[0] & 0x20 == 0 {
                return Err(differs(
                    at_dir,
                    "an entry set holds a critical entry that exFAT 1.00 does not name",
                ));
            }
        }

        let name = String::from_utf16(&units)
            .map_err(|_| differs(at_dir, "a name is not valid UTF-16"))?;
        let path = match above {
            Some(above) => above.join(&name)?,
            None => TreePath::new(&name)?,
        };
        let upcased = self.up.name(&units);
        let hash = u16::from_le_bytes([stream[4], stream[5]]);
        if name_hash(&upcased) != hash {
            return Err(differs(
                path.as_str(),
                "the name does not have the hash its entry records",
            ));
        }
        if !names.insert(upcased) {
            return Err(differs(
                path.as_str(),
                "the directory holds this name twice",
            ));
        }

        let u64_at = |at: usize| u64::from_le_bytes(stream[at..at + 8].try_into().unwrap());
        let data = Data {
            first: u32::from_le_bytes(stream[20..24].try_into().unwrap()),
            length: u64_at(24),
            valid: u64_at(8),
            contiguous: stream[1] & 2 != 0,
        };
        let attributes = u16::from_le_bytes([set[4], set[5]]);
        let is_dir = attributes & 0x10 != 0;
        if data.valid > data.length || (is_dir && data.valid != data.length) {
            return Err(differs(
                path.as_str(),
                "its valid length is not one that its length allows",
            ));
        }
        let runs = self.runs_of(&data, path.as_str())?;
        self.claim(&runs, path.as_str())?;
        Ok(Node { path, is_dir, data })
    }

    /// Give each piece of a file to `sink`, from its first byte to its last.
    pub(crate) fn read_file(&mut self, node: &Node, sink: &mut dyn FnMut(&[u8])) -> Result<()> {
        let runs = self.runs_of(&node.data, node.path.as_str())?;
        let mut done = 0u64;
        let mut buf = vec![0u8; CHUNK_BYTES as usize];
        for (first, clusters) in runs {
            let mut at = self.cluster_offset(first);
            let end = at + clusters as u64 * self.cluster;
            while at < end && done < node.data.length {
                let take = (end - at)
                    .min(CHUNK_BYTES)
                    .min((node.data.length - done).div_ceil(self.sector) * self.sector);
                let piece = &mut buf[..take as usize];
                self.target.seek(SeekFrom::Start(at))?;
                self.target.read_exact(piece)?;
                let keep = take.min(node.data.length - done) as usize;
                // The bytes past the valid length read as zero.
                if done + keep as u64 > node.data.valid {
                    let from = node.data.valid.saturating_sub(done) as usize;
                    piece[from..keep].fill(0);
                }
                sink(&piece[..keep]);
                done += keep as u64;
                at += take;
            }
        }
        Ok(())
    }

    /// Check that the bitmap marks each cluster that an allocation claims,
    /// and no other, and that the percent in use says the same.
    ///
    /// Call this after [`Volume::walk`], which claims the clusters of the
    /// tree.
    pub(crate) fn check_allocation(&self) -> Result<()> {
        let bit = |bytes: &[u8], n: u32| bytes[n as usize / 8] >> (n % 8) & 1 == 1;
        for n in 0..self.count {
            match (bit(&self.bitmap, n), bit(&self.claimed, n)) {
                (false, true) => {
                    return Err(invalid(format!(
                        "cluster {} is in use, and the bitmap marks it free",
                        n + 2
                    )))
                }
                (true, false) => {
                    return Err(invalid(format!(
                        "the bitmap marks cluster {} in use, and nothing uses it",
                        n + 2
                    )))
                }
                _ => {}
            }
        }
        let used: u64 = self.claimed.iter().map(|b| b.count_ones() as u64).sum();
        let percent = used * 100 / self.count as u64;
        if self.percent != 0xFF && self.percent as u64 != percent {
            return Err(invalid(format!(
                "its boot sector says {}% in use, and {percent}% is",
                self.percent
            )));
        }
        Ok(())
    }

    /// The runs of clusters that hold some data.
    fn runs_of(&mut self, data: &Data, path: &str) -> Result<Vec<Run>> {
        if data.first == 0 {
            if data.length != 0 {
                return Err(differs(path, "it has a length and no cluster"));
            }
            return Ok(Vec::new());
        }
        if !data.contiguous {
            return self.chain(data.first, Some(data.length));
        }
        let clusters = data.length.div_ceil(self.cluster);
        if clusters == 0
            || data.first < 2
            || data.first as u64 + clusters - 1 > self.count as u64 + 1
        {
            return Err(differs(
                path,
                "its clusters are not inside the cluster heap",
            ));
        }
        Ok(vec![(data.first, clusters as u32)])
    }

    /// The runs of a chain in the FAT, from `first` to its end.
    ///
    /// With a length, the chain has to hold exactly the clusters that the
    /// length needs.
    fn chain(&mut self, first: u32, length: Option<u64>) -> Result<Vec<Run>> {
        let mut runs: Vec<Run> = Vec::new();
        let mut cluster = first;
        let mut total = 0u64;
        loop {
            if cluster < 2 || cluster > self.count + 1 {
                return Err(invalid(format!(
                    "a chain in its FAT names cluster {cluster}"
                )));
            }
            total += 1;
            if total > self.count as u64 {
                return Err(invalid("a chain in its FAT never ends"));
            }
            match runs.last_mut() {
                Some((start, n)) if *start + *n == cluster => *n += 1,
                _ => runs.push((cluster, 1)),
            }
            let next = self.fat_entry(cluster)?;
            if next == END_OF_CHAIN {
                break;
            }
            cluster = next;
        }
        if let Some(length) = length {
            if total != length.div_ceil(self.cluster) {
                return Err(invalid(format!(
                    "a chain of {total} clusters holds {length} bytes"
                )));
            }
        }
        Ok(runs)
    }

    /// One entry of the FAT, read from the drive.
    fn fat_entry(&mut self, cluster: u32) -> io::Result<u32> {
        let at = self.fat_start + cluster as u64 * 4;
        let start = at / self.sector * self.sector;
        let mut sector = vec![0u8; self.sector as usize];
        self.target.seek(SeekFrom::Start(start))?;
        self.target.read_exact(&mut sector)?;
        let i = (at - start) as usize;
        Ok(u32::from_le_bytes(sector[i..i + 4].try_into().unwrap()))
    }

    /// Mark each cluster of the runs as claimed, and refuse a cluster that
    /// something else claimed first.
    fn claim(&mut self, runs: &[Run], what: &str) -> Result<()> {
        for &(first, clusters) in runs {
            for cluster in first..first + clusters {
                let n = (cluster - 2) as usize;
                if self.claimed[n / 8] >> (n % 8) & 1 == 1 {
                    return Err(differs(
                        what,
                        &format!("cluster {cluster} belongs to two things"),
                    ));
                }
                self.claimed[n / 8] |= 1 << (n % 8);
            }
        }
        Ok(())
    }

    /// The first `length` bytes of some runs.
    fn read_runs(&mut self, runs: &[Run], length: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(length as usize);
        for &(first, clusters) in runs {
            let mut run = vec![0u8; (clusters as u64 * self.cluster) as usize];
            self.target
                .seek(SeekFrom::Start(self.cluster_offset(first)))?;
            self.target.read_exact(&mut run)?;
            bytes.extend_from_slice(&run);
        }
        bytes.truncate(length as usize);
        Ok(bytes)
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        self.heap_start + (cluster as u64 - 2) * self.cluster
    }
}

/// Check one boot region: the boot sector, the signature of each extended
/// boot sector, and the checksum.
fn check_region(region: &[u8], sector: usize, what: &str) -> Result<()> {
    let boot = &region[..sector];
    if &boot[..11] != b"\xEB\x76\x90EXFAT   " || boot[510..512] != [0x55, 0xAA] {
        return Err(invalid(format!("{what} is not the boot region of exFAT")));
    }
    if boot[11..64].iter().any(|b| *b != 0) {
        return Err(invalid(format!("{what} has parameters of FAT in it")));
    }
    for number in 1..=8 {
        let end = (number + 1) * sector;
        if region[end - 4..end] != [0x00, 0x00, 0x55, 0xAA] {
            return Err(invalid(format!(
                "extended boot sector {number} of {what} has no signature"
            )));
        }
    }
    let sum = boot_checksum(&region[..11 * sector]);
    let recorded = &region[11 * sector..12 * sector];
    if recorded
        .chunks_exact(4)
        .any(|w| u32::from_le_bytes(w.try_into().unwrap()) != sum)
    {
        return Err(invalid(format!(
            "{what} does not have the checksum its last sector records"
        )));
    }
    Ok(())
}

/// The first cluster and the length that an entry of the root records.
fn stream_of(entry: &[u8]) -> (u32, u64) {
    (
        u32::from_le_bytes(entry[20..24].try_into().unwrap()),
        u64::from_le_bytes(entry[24..32].try_into().unwrap()),
    )
}

fn runs_clusters(runs: &[Run]) -> u64 {
    runs.iter().map(|(_, n)| *n as u64).sum()
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the exFAT volume is not valid: {}", detail.into()),
    ))
}

fn differs(path: &str, detail: &str) -> Error {
    Error::VolumeDiffers {
        path: path.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exfat::entries::label_entry;
    use crate::exfat::geometry::Geometry;
    use crate::exfat::place::Placement;
    use crate::exfat::{write_exfat, ExfatOptions};
    use crate::testing::{windows_like, Drive, MIB};
    use crate::tree::{Entry, FileSource, MemorySource};
    use std::io::{Read, Seek, Write};

    fn options() -> ExfatOptions<'static> {
        ExfatOptions {
            label: "Install",
            serial: 0x1234_ABCD,
            first_sector: 2048,
        }
    }

    fn written(sector: u32, tree: &MemorySource) -> Drive {
        let mut d = Drive::new(sector, 64 * MIB);
        write_exfat(d.partition(), tree, &options()).unwrap();
        d
    }

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

    fn read_all(volume: &mut Volume<impl BlockTarget>, node: &Node) -> Vec<u8> {
        let mut bytes = Vec::new();
        volume
            .read_file(node, &mut |piece| bytes.extend_from_slice(piece))
            .unwrap();
        bytes
    }

    /// Change bytes of the partition at `at`, under any cache, a whole
    /// sector at a time.
    fn poke(d: &mut Drive, at: u64, change: impl FnOnce(&mut [u8])) {
        let sector = d.sector() as u64;
        let start = MIB + at / sector * sector;
        let mut block = d.bytes()[start as usize..(start + 2 * sector) as usize].to_vec();
        change(&mut block[(MIB + at - start) as usize..]);
        d.strict.seek(SeekFrom::Start(start)).unwrap();
        d.strict.write_all(&block).unwrap();
    }

    fn error_of(d: &mut Drive) -> String {
        let result = Volume::open(d.partition()).and_then(|mut v| {
            v.walk()?;
            v.check_allocation()
        });
        result.unwrap_err().to_string()
    }

    #[test]
    fn the_tree_that_went_on_comes_back() {
        let tree = windows_like();
        for sector in [512, 4096] {
            let mut d = written(sector, &tree);
            let mut volume = Volume::open(d.partition()).unwrap();
            let nodes = volume.walk().unwrap();
            volume.check_allocation().unwrap();

            let want = tree.entries().unwrap();
            assert_eq!(nodes.len(), want.len());
            for (node, entry) in nodes.iter().zip(&want) {
                assert_eq!(&node.path, entry.path());
                match entry {
                    Entry::Dir(_) => assert!(node.is_dir, "{}", node.path),
                    Entry::File(path, bytes) => {
                        assert!(!node.is_dir);
                        assert_eq!(node.data.length, *bytes);
                        let mut source = Vec::new();
                        tree.open(path).unwrap().read_to_end(&mut source).unwrap();
                        assert_eq!(read_all(&mut volume, node), source, "{path}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_volume_that_is_not_exfat_is_refused() {
        let mut d = Drive::new(512, 64 * MIB);
        let e = Volume::open(d.partition()).err().unwrap().to_string();
        assert!(e.contains("is not the boot region of exFAT"), "{e}");
    }

    #[test]
    fn a_changed_byte_in_the_boot_sector_breaks_the_checksum() {
        let mut d = written(512, &windows_like());
        poke(&mut d, 100, |b| b[0] ^= 1);
        let e = error_of(&mut d);
        assert!(
            e.contains("the boot region does not have the checksum"),
            "{e}"
        );
    }

    #[test]
    fn a_copy_of_the_boot_region_that_differs_is_found() {
        let mut d = written(512, &windows_like());
        // A change to the copy and to its checksum: a valid region that is
        // not the same region.
        poke(&mut d, 12 * 512 + 100, |b| b[0] ^= 1);
        let mut region = d.partition_bytes()[12 * 512..24 * 512].to_vec();
        let sum = boot_checksum(&region[..11 * 512]);
        for w in region[11 * 512..].chunks_exact_mut(4) {
            w.copy_from_slice(&sum.to_le_bytes());
        }
        poke(&mut d, 23 * 512, |b| {
            b[..512].copy_from_slice(&region[11 * 512..])
        });
        let e = error_of(&mut d);
        assert!(
            e.contains("the copy of the boot region differs from it at byte 100"),
            "{e}"
        );
    }

    #[test]
    fn a_changed_byte_in_an_entry_set_breaks_its_checksum() {
        let tree = windows_like();
        let mut d = written(512, &tree);
        let p = placement(&d, &tree);
        // The first set in the root, after the label, the bitmap and the
        // up-case table. Byte 8 is its time of creation.
        let at = p.geometry.cluster_offset(p.root.first) + 3 * 32 + 8;
        poke(&mut d, at, |b| b[0] ^= 1);
        let e = error_of(&mut d);
        assert!(e.contains("does not have the checksum it records"), "{e}");
    }

    #[test]
    fn a_name_whose_hash_is_wrong_is_found() {
        let tree = windows_like();
        let mut d = written(512, &tree);
        let p = placement(&d, &tree);
        // The first set of the root is for a name of 27 units, so it holds
        // two name entries.
        let set_at = (p.geometry.cluster_offset(p.root.first) + 3 * 32) as usize;
        let length = (1 + d.partition_bytes()[set_at + 1] as usize) * 32;
        assert_eq!(length, 4 * 32);
        let mut set = d.partition_bytes()[set_at..set_at + length].to_vec();
        set[32 + 4] ^= 1;
        let sum = set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        poke(&mut d, set_at as u64, |b| b[..length].copy_from_slice(&set));
        let e = error_of(&mut d);
        assert!(
            e.contains("does not have the hash its entry records"),
            "{e}"
        );
    }

    #[test]
    fn a_cluster_that_two_files_claim_is_found() {
        let mut tree = MemorySource::new();
        tree.add_file("a", vec![1u8; 5000]).unwrap();
        tree.add_file("b", vec![2u8; 5000]).unwrap();
        let mut d = written(512, &tree);
        let p = placement(&d, &tree);
        // Point b at the first cluster of a, and fix the checksum of its set.
        let set_at = p.geometry.cluster_offset(p.root.first) + 6 * 32;
        let mut set = d.partition_bytes()[set_at as usize..set_at as usize + 96].to_vec();
        set[32 + 20..32 + 24].copy_from_slice(&p.items[0].run.first.to_le_bytes());
        let sum = set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        poke(&mut d, set_at, |b| b[..96].copy_from_slice(&set));
        let e = error_of(&mut d);
        assert!(e.contains("belongs to two things"), "{e}");
    }

    #[test]
    fn a_bitmap_that_marks_the_wrong_clusters_is_found() {
        let tree = windows_like();
        for (flip, want) in [
            (0usize, "is in use, and the bitmap marks it free"),
            (1000, "nothing uses it"),
        ] {
            let mut d = written(512, &tree);
            let p = placement(&d, &tree);
            let at = p.geometry.cluster_offset(p.bitmap.first) + (flip / 8) as u64;
            poke(&mut d, at, |b| b[0] ^= 1 << (flip % 8));
            let e = error_of(&mut d);
            assert!(e.contains(want), "{e}");
        }
    }

    #[test]
    fn an_up_case_table_that_changed_is_found() {
        let tree = windows_like();
        let mut d = written(512, &tree);
        let p = placement(&d, &tree);
        poke(
            &mut d,
            p.geometry.cluster_offset(p.upcase.first) + 200,
            |b| b[0] ^= 1,
        );
        let e = error_of(&mut d);
        assert!(
            e.contains("up-case table does not have the checksum"),
            "{e}"
        );
    }

    #[test]
    fn a_file_reads_as_zero_past_its_valid_length() {
        let mut tree = MemorySource::new();
        tree.add_file("a", vec![7u8; 3000]).unwrap();
        let mut d = written(512, &tree);
        let mut volume = Volume::open(d.partition()).unwrap();
        let mut node = volume.walk().unwrap().remove(0);
        node.data.valid = 1000;
        let bytes = read_all(&mut volume, &node);
        assert_eq!(bytes.len(), 3000);
        assert!(bytes[..1000].iter().all(|b| *b == 7));
        assert!(bytes[1000..].iter().all(|b| *b == 0));
    }
}
