//! The tree of a Windows ISO, split onto the two partitions of the layout.
//!
//! Partition 2 holds the install image alone. Partition 1 holds every other
//! file, and `autounattend.xml`. This finds the image and the architecture in
//! the tree, sizes partition 1 from the boot files, and checks that a drive
//! holds both partitions before a byte goes onto it.

use std::io::Read;

use burnout_core::{Entry, Error, FileSource, Result, TreePath};

use crate::exfat::check_exfat_fits;
use crate::plan::{plan, Layout, ALIGN};
use crate::unattend::{Architecture, InstallImage};

/// The file that Setup reads at the root of the boot partition.
pub const UNATTEND_FILE: &str = "autounattend.xml";

/// The label of partition 2. The search in `autounattend.xml` finds the
/// volume by what it holds, so no label is part of the layout.
pub const INSTALL_LABEL: &str = "INSTALL";

/// The cluster of the FAT32 volume on a partition of this size.
const CLUSTER: u64 = 4096;

/// Room on partition 1 past the boot files: the tables of FAT32, and a
/// driver that a person adds later.
const BOOT_ROOM: u64 = 256 * 1024 * 1024;

/// The UEFI boot files, and the architecture that each one starts.
const BOOT_FILES: [(&str, Architecture); 3] = [
    ("efi/boot/bootx64.efi", Architecture::Amd64),
    ("efi/boot/bootaa64.efi", Architecture::Arm64),
    ("efi/boot/bootia32.efi", Architecture::X86),
];

/// A Windows tree, split onto the two partitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsTree {
    /// Every entry but the install image and an `autounattend.xml` of the
    /// tree, in the order of the tree.
    boot: Vec<Entry>,
    image_path: TreePath,
    image_bytes: u64,
    pub image: InstallImage,
    pub architecture: Architecture,
    /// The tree holds an `autounattend.xml` of its own, and the one of
    /// Burnout takes its place, because Setup finds the install image only
    /// through that file.
    pub replaces_unattend: bool,
}

impl WindowsTree {
    /// Split the tree of a Windows ISO.
    pub fn new<S: FileSource + ?Sized>(source: &S) -> Result<Self> {
        let entries = source.entries()?;
        let lower = |e: &Entry| e.path().as_str().to_ascii_lowercase();

        let mut images = entries.iter().filter_map(|e| match (e, lower(e).as_str()) {
            (Entry::File(path, bytes), "sources/install.wim") => {
                Some((path.clone(), *bytes, InstallImage::Wim))
            }
            (Entry::File(path, bytes), "sources/install.esd") => {
                Some((path.clone(), *bytes, InstallImage::Esd))
            }
            _ => None,
        });
        let first = images.next();
        let (image_path, image_bytes, image) = match (first, images.next()) {
            (Some(image), None) => image,
            (None, _) => {
                return Err(not_windows(
                    "sources",
                    "the tree holds no sources/install.wim and no sources/install.esd",
                ))
            }
            (Some(_), Some(_)) => {
                return Err(not_windows(
                    "sources",
                    "the tree holds both sources/install.wim and sources/install.esd, \
                     and Burnout cannot tell which one Setup is to take",
                ))
            }
        };

        let found: Vec<Architecture> = BOOT_FILES
            .iter()
            .filter(|(file, _)| {
                entries
                    .iter()
                    .any(|e| matches!(e, Entry::File(..)) && lower(e) == *file)
            })
            .map(|(_, architecture)| *architecture)
            .collect();
        let architecture =
            match found.as_slice() {
                [one] => *one,
                [] => return Err(not_windows(
                    "efi/boot",
                    "the tree holds no UEFI boot file: bootx64.efi, bootaa64.efi or bootia32.efi. \
                     A drive of Windows mode starts from UEFI only",
                )),
                _ => {
                    return Err(not_windows(
                        "efi/boot",
                        "the tree holds UEFI boot files for more than one architecture",
                    ))
                }
            };

        let mut replaces_unattend = false;
        let boot = entries
            .into_iter()
            .filter(|e| {
                if e.path() == &image_path {
                    return false;
                }
                let own = matches!(e, Entry::File(..)) && lower(e) == UNATTEND_FILE;
                replaces_unattend |= own;
                !own
            })
            .collect();
        Ok(WindowsTree {
            boot,
            image_path,
            image_bytes,
            image,
            architecture,
            replaces_unattend,
        })
    }

    /// The install image, as the tree names it.
    pub fn image_path(&self) -> &TreePath {
        &self.image_path
    }

    pub fn image_bytes(&self) -> u64 {
        self.image_bytes
    }

    /// The files of partition 1, without `autounattend.xml`, and their
    /// bytes.
    pub fn boot_files(&self) -> (usize, u64) {
        self.boot.iter().fold((0, 0), |(n, bytes), e| match e {
            Entry::File(_, size) => (n + 1, bytes + size),
            Entry::Dir(_) => (n, bytes),
        })
    }

    /// The bytes that partition 1 takes: each boot file in whole clusters,
    /// a cluster for each directory and one for `autounattend.xml`, and room
    /// past them.
    pub fn boot_partition_bytes(&self) -> u64 {
        let used: u64 = self
            .boot
            .iter()
            .map(|e| match e {
                Entry::File(_, bytes) => bytes.div_ceil(CLUSTER) * CLUSTER,
                Entry::Dir(_) => CLUSTER,
            })
            .sum();
        (used + CLUSTER + BOOT_ROOM).div_ceil(ALIGN) * ALIGN
    }

    /// The entries of partition 2: the install image, and the directory
    /// that holds it.
    fn install_entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .image_path
            .parent()
            .map(Entry::Dir)
            .into_iter()
            .collect();
        entries.push(Entry::File(self.image_path.clone(), self.image_bytes));
        entries
    }

    /// The layout of a drive of `drive_bytes`, which has to hold both
    /// partitions.
    pub fn plan(&self, drive_bytes: u64, sector_size: u32) -> Result<Layout> {
        match self.fitting(drive_bytes, sector_size)? {
            Some(layout) => Ok(layout),
            None => Err(Error::NeedsLargerDrive {
                needed_bytes: self.needed_drive_bytes(sector_size)?,
                drive_bytes,
            }),
        }
    }

    /// The smallest drive, in whole mebibytes, that holds the layout.
    pub fn needed_drive_bytes(&self, sector_size: u32) -> Result<u64> {
        // Twice the image and partition 1 hold both partitions with room, so
        // the answer lies below that, and a halving finds it.
        let mut fits = 2 * (self.boot_partition_bytes() + self.image_bytes.div_ceil(ALIGN) * ALIGN);
        while self.fitting(fits, sector_size)?.is_none() {
            fits *= 2;
        }
        let mut short = 0;
        while fits - short > ALIGN {
            let middle = (short + fits) / 2 / ALIGN * ALIGN;
            match self.fitting(middle, sector_size)? {
                Some(_) => fits = middle,
                None => short = middle,
            }
        }
        Ok(fits)
    }

    /// The layout of a drive of `drive_bytes`, or `None` when the drive is
    /// too small for it.
    fn fitting(&self, drive_bytes: u64, sector_size: u32) -> Result<Option<Layout>> {
        let layout = match plan(drive_bytes, sector_size, self.boot_partition_bytes()) {
            Ok(layout) => layout,
            Err(Error::TooSmall { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let entries = self.install_entries();
        match check_exfat_fits(layout.install.length, sector_size, INSTALL_LABEL, &entries) {
            Ok(()) => Ok(Some(layout)),
            Err(Error::VolumeFull { .. } | Error::PartitionTooSmall { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The tree of partition 1: every boot file of `source`, and
    /// `autounattend.xml` with these bytes.
    pub fn boot_source<'a, S: FileSource + ?Sized>(
        &'a self,
        source: &'a S,
        unattend: &'a [u8],
    ) -> BootSource<'a, S> {
        let file = TreePath::new(UNATTEND_FILE).expect("a name");
        let mut entries = self.boot.clone();
        entries.push(Entry::File(file.clone(), unattend.len() as u64));
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        BootSource {
            source,
            entries,
            file,
            unattend,
        }
    }

    /// The tree of partition 2: the install image of `source`.
    pub fn install_source<'a, S: FileSource + ?Sized>(
        &'a self,
        source: &'a S,
    ) -> InstallSource<'a, S> {
        InstallSource {
            source,
            entries: self.install_entries(),
            image: &self.image_path,
        }
    }
}

fn not_windows(path: &str, detail: &str) -> Error {
    Error::Source {
        path: path.to_string(),
        detail: detail.to_string(),
    }
}

/// The boot files of a tree, and `autounattend.xml`.
pub struct BootSource<'a, S: ?Sized> {
    source: &'a S,
    entries: Vec<Entry>,
    file: TreePath,
    unattend: &'a [u8],
}

impl<S: FileSource + ?Sized> FileSource for BootSource<'_, S> {
    fn entries(&self) -> Result<Vec<Entry>> {
        Ok(self.entries.clone())
    }

    fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>> {
        if *path == self.file {
            return Ok(Box::new(self.unattend));
        }
        self.source.open(path)
    }
}

/// The install image of a tree, alone.
pub struct InstallSource<'a, S: ?Sized> {
    source: &'a S,
    entries: Vec<Entry>,
    image: &'a TreePath,
}

impl<S: FileSource + ?Sized> FileSource for InstallSource<'_, S> {
    fn entries(&self) -> Result<Vec<Entry>> {
        Ok(self.entries.clone())
    }

    fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>> {
        if path != self.image {
            return Err(Error::Source {
                path: path.to_string(),
                detail: "partition 2 holds the install image alone".to_string(),
            });
        }
        self.source.open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::windows_like;
    use burnout_core::MemorySource;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    /// A tree of entries and their sizes, with no bytes behind them.
    struct Listed(Vec<Entry>);

    impl FileSource for Listed {
        fn entries(&self) -> Result<Vec<Entry>> {
            Ok(self.0.clone())
        }

        fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>> {
            unreachable!("{path} is only listed")
        }
    }

    fn dir(path: &str) -> Entry {
        Entry::Dir(TreePath::new(path).unwrap())
    }

    fn file(path: &str, bytes: u64) -> Entry {
        Entry::File(TreePath::new(path).unwrap(), bytes)
    }

    fn windows(image: &str, image_bytes: u64) -> Listed {
        let mut entries = vec![
            dir("efi"),
            dir("efi/boot"),
            file("efi/boot/bootx64.efi", 2_000_000),
            dir("sources"),
            file("sources/boot.wim", 700 * MIB),
            file(image, image_bytes),
            file("setup.exe", 100_000),
        ];
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        Listed(entries)
    }

    fn paths(source: &impl FileSource) -> Vec<String> {
        let entries = source.entries().unwrap();
        entries.iter().map(|e| e.path().to_string()).collect()
    }

    #[test]
    fn the_install_image_goes_alone_onto_partition_2() {
        let tree = windows("sources/install.wim", 7 * GIB);
        let split = WindowsTree::new(&tree).unwrap();
        assert_eq!(
            (split.image, split.architecture),
            (InstallImage::Wim, Architecture::Amd64)
        );
        assert_eq!(split.image_bytes(), 7 * GIB);
        assert_eq!(
            paths(&split.install_source(&tree)),
            ["sources", "sources/install.wim"]
        );
        assert_eq!(
            paths(&split.boot_source(&tree, b"<unattend/>")),
            [
                "autounattend.xml",
                "efi",
                "efi/boot",
                "efi/boot/bootx64.efi",
                "setup.exe",
                "sources",
                "sources/boot.wim"
            ]
        );
        assert_eq!(split.boot_files(), (3, 2_000_000 + 700 * MIB + 100_000));
    }

    #[test]
    fn an_esd_image_and_the_case_of_a_name_are_found() {
        let split = WindowsTree::new(&windows("sources/install.esd", GIB)).unwrap();
        assert_eq!(split.image, InstallImage::Esd);
        let mut tree = windows("sources/x.txt", 1);
        tree.0.push(file("Sources/Install.WIM", GIB));
        tree.0.sort_by(|a, b| a.path().cmp(b.path()));
        let split = WindowsTree::new(&tree).unwrap();
        assert_eq!(split.image_path().as_str(), "Sources/Install.WIM");
    }

    #[test]
    fn the_boot_file_gives_the_architecture() {
        for (boot, architecture) in [
            ("efi/boot/BOOTAA64.EFI", Architecture::Arm64),
            ("efi/boot/bootia32.efi", Architecture::X86),
        ] {
            let mut tree = windows("sources/install.wim", GIB);
            tree.0.retain(|e| !e.path().as_str().ends_with(".efi"));
            tree.0.push(file(boot, 1000));
            assert_eq!(WindowsTree::new(&tree).unwrap().architecture, architecture);
        }
    }

    #[test]
    fn a_tree_that_is_not_windows_is_refused_with_the_reason() {
        let refused = |tree: &Listed| WindowsTree::new(tree).unwrap_err().to_string();
        let e = refused(&windows("sources/other.wim", GIB));
        assert!(
            e.contains("no sources/install.wim and no sources/install.esd"),
            "{e}"
        );
        let mut both = windows("sources/install.wim", GIB);
        both.0.push(file("sources/install.esd", GIB));
        assert!(refused(&both).contains("both sources/install.wim and"));
        let mut no_boot = windows("sources/install.wim", GIB);
        no_boot.0.retain(|e| !e.path().as_str().ends_with(".efi"));
        assert!(refused(&no_boot).contains("no UEFI boot file"));
        let mut two = windows("sources/install.wim", GIB);
        two.0.push(file("efi/boot/bootaa64.efi", 1000));
        assert!(refused(&two).contains("more than one architecture"));
    }

    #[test]
    fn an_unattend_file_of_the_tree_gives_way_to_the_one_of_burnout() {
        let mut tree = MemorySource::new();
        tree.add_file("efi/boot/bootx64.efi", b"efi".to_vec())
            .unwrap();
        tree.add_file("sources/install.wim", b"wim".to_vec())
            .unwrap();
        tree.add_file("AutoUnattend.xml", b"theirs".to_vec())
            .unwrap();
        let split = WindowsTree::new(&tree).unwrap();
        assert!(split.replaces_unattend);
        let boot = split.boot_source(&tree, b"ours");
        let names = paths(&boot);
        assert_eq!(
            names
                .iter()
                .filter(|n| n.eq_ignore_ascii_case(UNATTEND_FILE))
                .count(),
            1
        );
        let mut bytes = Vec::new();
        boot.open(&TreePath::new(UNATTEND_FILE).unwrap())
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"ours");
        let mut efi = Vec::new();
        boot.open(&TreePath::new("efi/boot/bootx64.efi").unwrap())
            .unwrap()
            .read_to_end(&mut efi)
            .unwrap();
        assert_eq!(efi, b"efi");
    }

    #[test]
    fn partition_2_opens_the_install_image_and_nothing_else() {
        let mut tree = windows_like();
        tree.add_file("sources/install.wim", b"MSWIM".to_vec())
            .unwrap();
        let split = WindowsTree::new(&tree).unwrap();
        let install = split.install_source(&tree);
        assert!(install
            .open(&TreePath::new("sources/install.wim").unwrap())
            .is_ok());
        let e = install
            .open(&TreePath::new("sources/boot.wim").unwrap())
            .err()
            .unwrap();
        assert!(e.to_string().contains("the install image alone"), "{e}");
    }

    #[test]
    fn partition_1_holds_the_boot_files_in_clusters_and_room_past_them() {
        let split = WindowsTree::new(&windows("sources/install.wim", GIB)).unwrap();
        // Three directories, three files in whole clusters, and one cluster
        // for autounattend.xml.
        let used = 3 * 4096 + 2_002_944 + 700 * MIB + 102_400 + 4096;
        assert_eq!(
            split.boot_partition_bytes(),
            (used + 256 * MIB).div_ceil(MIB) * MIB
        );
    }

    #[test]
    fn the_smallest_drive_holds_the_layout_and_one_mebibyte_less_does_not() {
        let split = WindowsTree::new(&windows("sources/install.wim", 7 * GIB)).unwrap();
        for sector in [512, 4096] {
            let needed = split.needed_drive_bytes(sector).unwrap();
            let layout = split.plan(needed, sector).unwrap();
            assert_eq!(layout.boot.length, split.boot_partition_bytes());
            assert!(layout.install.length >= 7 * GIB);
            let e = split.plan(needed - MIB, sector).unwrap_err();
            assert!(
                matches!(e, Error::NeedsLargerDrive { needed_bytes, drive_bytes }
                    if needed_bytes == needed && drive_bytes == needed - MIB),
                "{e}"
            );
        }
    }

    #[test]
    fn a_drive_of_16_gb_holds_the_layout_of_a_windows_11_iso() {
        // The sizes of Windows 11 25H2 x64: 975 boot files of 849 MiB, and
        // install.wim of 7,578,075,168 bytes.
        let mut entries = vec![
            dir("efi"),
            dir("efi/boot"),
            file("efi/boot/bootx64.efi", 2_000_000),
        ];
        entries.push(dir("sources"));
        entries.push(file("sources/boot.wim", 849 * MIB));
        entries.push(file("sources/install.wim", 7_578_075_168));
        let split = WindowsTree::new(&Listed(entries)).unwrap();
        let needed = split.needed_drive_bytes(512).unwrap();
        assert!(needed < 9 * GIB, "{needed}");
        assert!(split.plan(16_000_000_000 / 512 * 512, 512).is_ok());
        assert!(split.plan(8_000_000_000 / 512 * 512, 512).is_err());
    }
}
