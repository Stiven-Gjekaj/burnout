//! Where each part of a tree goes on an exFAT volume.
//!
//! The writer knows the whole tree before it writes a byte, so this plans
//! every cluster first. Each directory and each file gets one run of
//! clusters, in the order of the tree, after the bitmap, the up-case table
//! and the root directory. A tree that cannot go onto the volume is refused
//! here, before anything is written.

use std::collections::HashMap;

use burnout_core::{Error, Result};

use super::entries::{
    bitmap_entry, file_set, name_units, set_entries, upcase_entry, Kind, ENTRY_BYTES,
};
use super::geometry::{Geometry, FIRST_CLUSTER};
use super::sums::name_hash;
use super::upcase::{recommended_bytes, UpCase, RECOMMENDED_CHECKSUM};
use crate::tree::{Entry, TreePath};

/// The largest directory of exFAT, in bytes of entries.
pub(crate) const MAX_DIRECTORY_BYTES: u64 = 256 * 1024 * 1024;

/// A run of clusters. A run of no clusters starts at cluster 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Run {
    pub first: u32,
    pub clusters: u32,
}

impl Run {
    /// One past the last cluster of the run.
    pub(crate) fn end(&self) -> u32 {
        self.first + self.clusters
    }
}

/// One directory or file of the tree, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub path: TreePath,
    pub kind: Kind,
    /// The name as it goes onto the volume.
    pub name: Vec<u16>,
    /// The hash of the name in upper case.
    pub hash: u16,
    pub run: Run,
    /// The bytes that the entry records: the size of a file, or every byte
    /// of the clusters of a directory.
    pub data_length: u64,
    /// The items that a directory holds, in the order of the tree.
    pub children: Vec<usize>,
}

/// Where everything goes on one volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    pub geometry: Geometry,
    /// The entry of the label, when the volume has a label.
    pub label: Option<[u8; ENTRY_BYTES]>,
    pub bitmap: Run,
    pub upcase: Run,
    pub root: Run,
    /// The items that the root holds, in the order of the tree.
    pub root_children: Vec<usize>,
    /// Every item, in the order of the tree.
    pub items: Vec<Item>,
}

impl Placement {
    /// Place every entry of a tree, in the order that the tree gives.
    ///
    /// A directory comes before what it holds, as a [`crate::FileSource`]
    /// gives it.
    pub(crate) fn new(
        geometry: Geometry,
        label: Option<[u8; ENTRY_BYTES]>,
        entries: &[Entry],
        up: &UpCase,
    ) -> Result<Self> {
        let mut items: Vec<Item> = Vec::with_capacity(entries.len());
        let mut root_children = Vec::new();
        let mut dirs: HashMap<&TreePath, usize> = HashMap::new();
        // The first path that took each name, in upper case, in each
        // directory.
        let mut taken: HashMap<(Option<usize>, Vec<u16>), &TreePath> = HashMap::new();

        for entry in entries {
            let path = entry.path();
            let name = name_units(path.name()).map_err(|detail| cannot_copy(path, detail))?;
            let upcased = up.name(&name);
            let hash = name_hash(&upcased);

            let parent = match path.parent() {
                None => None,
                Some(above) => Some(*dirs.get(&above).ok_or_else(|| {
                    cannot_copy(path, format!("the tree holds no directory {above}"))
                })?),
            };
            if let Some(first) = taken.get(&(parent, upcased.clone())) {
                return Err(cannot_copy(
                    path,
                    format!("exFAT does not tell its name apart from {first}"),
                ));
            }
            taken.insert((parent, upcased), path);

            let at = items.len();
            match parent {
                Some(dir) => items[dir].children.push(at),
                None => root_children.push(at),
            }
            let (kind, data_length) = match entry {
                Entry::Dir(_) => {
                    dirs.insert(path, at);
                    (Kind::Dir, 0)
                }
                Entry::File(_, bytes) => (Kind::File, *bytes),
            };
            items.push(Item {
                path: path.clone(),
                kind,
                name,
                hash,
                run: Run::default(),
                data_length,
                children: Vec::new(),
            });
        }

        let mut placement = Placement {
            geometry,
            label,
            bitmap: Run::default(),
            upcase: Run::default(),
            root: Run::default(),
            root_children,
            items,
        };
        placement.allocate()?;
        Ok(placement)
    }

    /// Give each part its run of clusters, one after the other.
    fn allocate(&mut self) -> Result<()> {
        let g = self.geometry;
        let mut clusters: Vec<u64> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            clusters.push(match item.kind {
                Kind::File => g.clusters_for(item.data_length),
                Kind::Dir => {
                    let bytes = self.entry_bytes(&item.children);
                    if bytes > MAX_DIRECTORY_BYTES {
                        return Err(cannot_copy(
                            &item.path,
                            format!(
                                "an exFAT directory holds {MAX_DIRECTORY_BYTES} bytes of entries or fewer, and this one needs {bytes}"
                            ),
                        ));
                    }
                    g.clusters_for(bytes).max(1)
                }
            });
        }
        let root_bytes = self.root_entry_bytes();
        let bitmap = g.clusters_for(g.bitmap_bytes());
        let upcase = g.clusters_for(recommended_bytes().len() as u64);
        let root = g.clusters_for(root_bytes).max(1);

        let needed = bitmap + upcase + root + clusters.iter().sum::<u64>();
        if needed > g.cluster_count as u64 {
            return Err(Error::VolumeFull {
                file_system: "exFAT",
                needed_bytes: needed * g.cluster_bytes(),
                free_bytes: g.cluster_count as u64 * g.cluster_bytes(),
            });
        }

        // Every count now fits in the heap, and so in 32 bits.
        let mut next = FIRST_CLUSTER;
        let mut take = |count: u64| {
            let count = count as u32;
            let run = Run {
                first: if count == 0 { 0 } else { next },
                clusters: count,
            };
            next += count;
            run
        };
        self.bitmap = take(bitmap);
        self.upcase = take(upcase);
        self.root = take(root);
        for (item, count) in self.items.iter_mut().zip(clusters) {
            item.run = take(count);
            if item.kind == Kind::Dir {
                item.data_length = count * g.cluster_bytes();
            }
        }
        Ok(())
    }

    /// The bytes of the entry sets of these items.
    fn entry_bytes(&self, children: &[usize]) -> u64 {
        children
            .iter()
            .map(|at| (set_entries(self.items[*at].name.len()) * ENTRY_BYTES) as u64)
            .sum()
    }

    /// The bytes of the entries of the root: the label, the bitmap and the
    /// up-case table, and then what the root holds.
    fn root_entry_bytes(&self) -> u64 {
        let own = if self.label.is_some() { 3 } else { 2 };
        (own * ENTRY_BYTES) as u64 + self.entry_bytes(&self.root_children)
    }

    /// The clusters in use: every cluster from the first to the last run.
    pub(crate) fn used_clusters(&self) -> u64 {
        let last = self
            .items
            .iter()
            .map(|item| item.run.end())
            .chain([self.root.end()])
            .max()
            .unwrap_or(FIRST_CLUSTER);
        (last - FIRST_CLUSTER) as u64
    }

    /// The part of the cluster heap in use, in percent, rounded down.
    pub(crate) fn percent_in_use(&self) -> u8 {
        (self.used_clusters() * 100 / self.geometry.cluster_count as u64) as u8
    }

    /// The clusters of the root directory, entries first and zero after.
    pub(crate) fn root_directory(&self) -> Vec<u8> {
        let table = recommended_bytes();
        let mut bytes = Vec::new();
        if let Some(label) = self.label {
            bytes.extend_from_slice(&label);
        }
        bytes.extend_from_slice(&bitmap_entry(
            self.bitmap.first,
            self.geometry.bitmap_bytes(),
        ));
        bytes.extend_from_slice(&upcase_entry(
            RECOMMENDED_CHECKSUM,
            self.upcase.first,
            table.len() as u64,
        ));
        self.sets_into(&mut bytes, &self.root_children);
        bytes.resize(
            (self.root.clusters as u64 * self.geometry.cluster_bytes()) as usize,
            0,
        );
        bytes
    }

    /// The clusters of a directory of the tree, entries first and zero
    /// after.
    pub(crate) fn directory(&self, item: &Item) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.sets_into(&mut bytes, &item.children);
        bytes.resize(item.data_length as usize, 0);
        bytes
    }

    fn sets_into(&self, bytes: &mut Vec<u8>, children: &[usize]) {
        for at in children {
            let child = &self.items[*at];
            bytes.extend_from_slice(&file_set(
                &child.name,
                child.hash,
                child.kind,
                child.run.first,
                child.data_length,
            ));
        }
    }
}

/// The smallest volume that holds an exFAT file system with nothing on it.
pub(crate) fn min_volume_bytes(sector_bytes: u32) -> u64 {
    let up = UpCase::recommended();
    let mut bytes = 1024 * 1024;
    loop {
        if let Some(g) = Geometry::new(bytes, sector_bytes) {
            if Placement::new(g, None, &[], &up).is_ok() {
                return bytes;
            }
        }
        bytes += sector_bytes as u64;
    }
}

fn cannot_copy(path: &TreePath, detail: String) -> Error {
    Error::CannotCopy {
        path: path.to_string(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exfat::entries::label_entry;
    use crate::testing::{windows_like, MIB};
    use crate::tree::{FileSource, MemorySource};

    const GIB: u64 = 1024 * MIB;

    fn place(volume: u64, sector: u32, entries: &[Entry]) -> Result<Placement> {
        let g = Geometry::new(volume, sector).unwrap();
        Placement::new(g, label_entry("Install"), entries, &UpCase::recommended())
    }

    fn dir(path: &str) -> Entry {
        Entry::Dir(TreePath::new(path).unwrap())
    }

    fn file(path: &str, bytes: u64) -> Entry {
        Entry::File(TreePath::new(path).unwrap(), bytes)
    }

    #[test]
    fn the_runs_follow_each_other_from_cluster_2_in_the_order_of_the_tree() {
        for sector in [512, 4096] {
            let p = place(64 * MIB, sector, &windows_like().entries().unwrap()).unwrap();
            assert_eq!(p.bitmap.first, 2);
            assert_eq!(p.upcase.first, p.bitmap.end());
            assert_eq!(p.root.first, p.upcase.end());
            let mut next = p.root.end();
            for item in &p.items {
                if item.run.clusters > 0 {
                    assert_eq!(item.run.first, next, "{}", item.path);
                    next = item.run.end();
                }
            }
            assert_eq!(p.used_clusters(), (next - 2) as u64);
        }
    }

    #[test]
    fn the_up_case_table_takes_two_clusters_of_4_kib() {
        let p = place(64 * MIB, 512, &[]).unwrap();
        assert_eq!(p.geometry.cluster_bytes(), 4096);
        assert_eq!(p.upcase.clusters, 2);
        assert_eq!(p.root.clusters, 1);
    }

    #[test]
    fn a_directory_takes_a_cluster_even_when_it_is_empty() {
        let p = place(64 * MIB, 512, &[dir("support")]).unwrap();
        assert_eq!(p.items[0].run.clusters, 1);
        assert_eq!(p.items[0].data_length, 4096);
    }

    #[test]
    fn a_file_of_no_bytes_takes_no_cluster() {
        let p = place(64 * MIB, 512, &[file("autorun.inf", 0)]).unwrap();
        assert_eq!(
            p.items[0].run,
            Run {
                first: 0,
                clusters: 0
            }
        );
    }

    #[test]
    fn a_directory_with_many_entries_takes_more_than_one_cluster() {
        let mut tree = MemorySource::new();
        for n in 0..200 {
            tree.add_file(&format!("many/file number {n:03}.txt"), *b"x")
                .unwrap();
        }
        let p = place(64 * MIB, 512, &tree.entries().unwrap()).unwrap();
        // Each name of 19 units takes two name entries, so each set is four
        // entries of 32 bytes.
        let many = &p.items[0];
        assert_eq!(many.run.clusters, (200 * 4 * 32u64).div_ceil(4096) as u32);
        assert_eq!(p.directory(many).len() as u64, many.data_length);
    }

    #[test]
    fn a_file_over_4_gib_gets_every_cluster_it_needs() {
        let bytes = 6 * GIB + 12_345;
        let p = place(
            6 * GIB + 200 * MIB,
            512,
            &[dir("sources"), file("sources/install.wim", bytes)],
        )
        .unwrap();
        let wim = &p.items[1];
        assert_eq!(p.geometry.cluster_bytes(), 32 * 1024);
        assert_eq!(wim.run.clusters as u64, bytes.div_ceil(32 * 1024));
        assert_eq!(wim.data_length, bytes);
        assert!(p.percent_in_use() >= 96, "{}", p.percent_in_use());
    }

    #[test]
    fn a_tree_that_does_not_fit_is_refused_with_what_it_needs() {
        let e = place(64 * MIB, 512, &[file("install.wim", 100 * MIB)]).unwrap_err();
        match e {
            Error::VolumeFull {
                needed_bytes,
                free_bytes,
                ..
            } => {
                assert!(needed_bytes > 100 * MIB);
                assert!(free_bytes < 64 * MIB);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn names_that_differ_only_in_case_are_refused() {
        let e = place(64 * MIB, 512, &[dir("EFI"), dir("efi")]).unwrap_err();
        assert_eq!(
            e.to_string(),
            "cannot copy efi onto the volume: exFAT does not tell its name apart from EFI"
        );
        // The same rule holds past ASCII, because the up-case table says so.
        let e = place(
            64 * MIB,
            512,
            &[file("\u{dc}.txt", 1), file("\u{fc}.txt", 1)],
        )
        .unwrap_err();
        assert!(e.to_string().contains("apart from \u{dc}.txt"), "{e}");
    }

    #[test]
    fn the_same_name_in_two_directories_is_two_names() {
        let entries = [dir("a"), dir("b"), file("a/x", 1), file("b/x", 1)];
        assert!(place(64 * MIB, 512, &entries).is_ok());
    }

    #[test]
    fn a_name_that_exfat_cannot_hold_is_refused_with_its_path() {
        match place(64 * MIB, 512, &[dir("efi"), file("efi/a:b", 1)]) {
            Err(Error::CannotCopy { path, .. }) => assert_eq!(path, "efi/a:b"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_entry_in_a_directory_that_the_tree_does_not_name_is_refused() {
        let e = place(64 * MIB, 512, &[file("a/b", 1)]).unwrap_err();
        assert!(e.to_string().contains("no directory a"), "{e}");
        // A file is not a directory either.
        let e = place(64 * MIB, 512, &[file("a", 1), file("a/b", 1)]).unwrap_err();
        assert!(e.to_string().contains("no directory a"), "{e}");
    }

    #[test]
    fn the_root_starts_with_the_label_the_bitmap_and_the_up_case_table() {
        let p = place(64 * MIB, 512, &[file("setup.exe", 10)]).unwrap();
        let root = p.root_directory();
        assert_eq!(root.len(), 4096);
        assert_eq!(
            &root[..3 * 32]
                .iter()
                .step_by(32)
                .copied()
                .collect::<Vec<_>>(),
            &[0x83, 0x81, 0x82]
        );
        assert_eq!(root[3 * 32], 0x85, "then the set of setup.exe");
        assert!(root[6 * 32..].iter().all(|b| *b == 0), "then the end");
    }

    #[test]
    fn the_smallest_volume_holds_nothing_and_one_sector_less_holds_less() {
        for sector in [512u32, 4096] {
            let smallest = min_volume_bytes(sector);
            assert!(place(smallest, sector, &[]).is_ok());
            let g = Geometry::new(smallest - sector as u64, sector);
            let smaller = g.map(|g| Placement::new(g, None, &[], &UpCase::recommended()));
            assert!(!matches!(smaller, Some(Ok(_))), "{sector}");
        }
    }
}
