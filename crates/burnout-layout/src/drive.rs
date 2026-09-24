//! Write a Windows drive, and check it.
//!
//! The write puts the table, both volumes and every file onto the drive,
//! and flushes it. The check comes after, through a new handle onto the
//! drive, so it reads the drive and not a cache of the host. It reads the
//! table and each file back through a new mount of its volume, and compares
//! each file with the digest that it went in with. A hash of the whole drive
//! would prove nothing here, because the source never held these volumes.

use std::io::SeekFrom;

use burnout_core::{
    BlockTarget, Error, FileSource, Progress, ProgressEvent, Result, Stage, Window,
};

use crate::copy::copy_to_fat32_with;
use crate::exfat::{verify_exfat_with, write_exfat_with, ExfatOptions};
use crate::fat32::{format_fat32, Fat32Options};
use crate::manifest::Manifest;
use crate::mbr::{mbr_bytes, write_table};
use crate::plan::{Extent, Layout};
use crate::verify::verify_fat32_with;
use crate::windows::{WindowsTree, INSTALL_LABEL};

/// The numbers that make each drive its own. Windows tells disks apart by
/// the signature of the table, and volumes by their serials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Serials {
    pub disk: u32,
    pub boot: u32,
    pub install: u32,
}

/// What goes onto a Windows drive besides its tree.
#[derive(Clone, Copy, Debug)]
pub struct WindowsDrive<'a> {
    pub layout: Layout,
    /// The label of partition 1.
    pub label: &'a str,
    pub serials: Serials,
    /// The bytes of `autounattend.xml`.
    pub unattend: &'a [u8],
}

/// What went onto each partition, which the check reads back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub boot: Manifest,
    pub install: Manifest,
}

impl Written {
    /// The files of both partitions, and their bytes.
    pub fn files(&self) -> (usize, u64) {
        let files = self.boot.files.iter().chain(&self.install.files);
        files.fold((0, 0), |(n, bytes), f| (n + 1, bytes + f.bytes))
    }
}

/// One partition of the drive.
fn partition<T: BlockTarget>(drive: &mut T, extent: Extent) -> Result<Window<&mut T>> {
    Window::new(drive, extent.start, extent.length)
}

/// Write the table, both volumes and every file of `tree`, and flush the
/// drive.
pub fn write_windows<T, S>(
    drive: &mut T,
    tree: &WindowsTree,
    source: &S,
    plan: &WindowsDrive,
    progress: &mut dyn Progress,
) -> Result<Written>
where
    T: BlockTarget,
    S: FileSource + ?Sized,
{
    let layout = &plan.layout;
    let total = tree.boot_files().1 + plan.unattend.len() as u64 + tree.image_bytes();
    progress.report(ProgressEvent::Start {
        stage: Stage::Write,
        total_bytes: Some(total),
    });
    let mut done = 0;
    let written = {
        let mut tally = |n: u64| {
            done += n;
            progress.report(ProgressEvent::Advance {
                stage: Stage::Write,
                bytes_done: done,
            });
        };
        write_table(drive, layout, plan.serials.disk)?;
        let fat32 = Fat32Options {
            label: plan.label,
            serial: plan.serials.boot,
            first_sector: layout.first_sector(layout.boot) as u32,
        };
        format_fat32(partition(drive, layout.boot)?, &fat32)?;
        let boot_source = tree.boot_source(source, plan.unattend);
        let boot = copy_to_fat32_with(partition(drive, layout.boot)?, &boot_source, &mut tally)?;
        let exfat = ExfatOptions {
            label: INSTALL_LABEL,
            serial: plan.serials.install,
            first_sector: layout.first_sector(layout.install),
        };
        let install_source = tree.install_source(source);
        let install = write_exfat_with(
            partition(drive, layout.install)?,
            &install_source,
            &exfat,
            &mut tally,
        )?;
        Written { boot, install }
    };

    // A byte count that reached the size of the tree is not a finished
    // write, so the flush comes first and the end of the write after it.
    progress.report(ProgressEvent::Start {
        stage: Stage::Flush,
        total_bytes: None,
    });
    drive.sync()?;
    progress.report(ProgressEvent::Done {
        stage: Stage::Flush,
        bytes_done: done,
    });
    progress.report(ProgressEvent::Done {
        stage: Stage::Write,
        bytes_done: done,
    });
    Ok(written)
}

/// Read the table and each file back from a new handle onto the drive, and
/// compare each with what went onto it.
pub fn verify_windows<T: BlockTarget>(
    drive: &mut T,
    plan: &WindowsDrive,
    written: &Written,
    progress: &mut dyn Progress,
) -> Result<()> {
    let layout = &plan.layout;
    let mut first = vec![0u8; layout.sector_size as usize];
    drive.seek(SeekFrom::Start(0))?;
    drive.read_exact(&mut first)?;
    if first[..512] != mbr_bytes(layout, plan.serials.disk) {
        return Err(Error::VolumeDiffers {
            path: "the partition table".to_string(),
            detail: "sector 0 of the drive does not hold the table that Burnout wrote".to_string(),
        });
    }

    let (_, total) = written.files();
    progress.report(ProgressEvent::Start {
        stage: Stage::Verify,
        total_bytes: Some(total),
    });
    let mut done = 0;
    {
        let mut tally = |n: u64| {
            done += n;
            progress.report(ProgressEvent::Advance {
                stage: Stage::Verify,
                bytes_done: done,
            });
        };
        verify_fat32_with(partition(drive, layout.boot)?, &written.boot, &mut tally)?;
        verify_exfat_with(
            partition(drive, layout.install)?,
            &written.install,
            &mut tally,
        )?;
    }
    progress.report(ProgressEvent::Done {
        stage: Stage::Verify,
        bytes_done: done,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbr::{TYPE_EXFAT, TYPE_FAT32};
    use crate::testing::{pattern, windows_like};
    use crate::windows::UNATTEND_FILE;
    use burnout_core::{FileTarget, MemorySource, StrictTarget, TreePath};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Seek, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    const SERIALS: Serials = Serials {
        disk: 0x4255_524E,
        boot: 0x0B0B_0B0B,
        install: 0x0E0E_0E0E,
    };

    /// A sparse file that goes away with the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static COUNT: AtomicU32 = AtomicU32::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            let name = format!("burnout-drive-{}-{n}.img", std::process::id());
            Scratch(std::env::temp_dir().join(name))
        }

        fn open(&self, sector: u32) -> StrictTarget<FileTarget> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.0)
                .unwrap();
            StrictTarget::new(FileTarget::new(file, sector).unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn tree() -> MemorySource {
        let mut tree = windows_like();
        tree.add_file("sources/install.wim", pattern(5 * 1024 * 1024 + 11, 9))
            .unwrap();
        tree
    }

    /// A drive just large enough for the tree, written.
    fn written(
        sector: u32,
        events: &mut Vec<ProgressEvent>,
    ) -> (Scratch, WindowsTree, Written, Layout) {
        let source = tree();
        let split = WindowsTree::new(&source).unwrap();
        let bytes = split.needed_drive_bytes(sector).unwrap();
        let scratch = Scratch::new();
        drop(FileTarget::create(&scratch.0, bytes, sector).unwrap());
        let layout = split.plan(bytes, sector).unwrap();
        let plan = WindowsDrive {
            layout,
            label: "CCCOMA_X64FRE",
            serials: SERIALS,
            unattend: b"<unattend/>",
        };
        let mut drive = scratch.open(sector);
        let written =
            write_windows(&mut drive, &split, &source, &plan, &mut |e| events.push(e)).unwrap();
        (scratch, split, written, layout)
    }

    fn plan_of(layout: Layout) -> WindowsDrive<'static> {
        WindowsDrive {
            layout,
            label: "CCCOMA_X64FRE",
            serials: SERIALS,
            unattend: b"<unattend/>",
        }
    }

    #[test]
    fn each_file_goes_onto_its_partition_and_the_check_passes() {
        for sector in [512, 4096] {
            let (scratch, split, written, layout) = written(sector, &mut Vec::new());
            let boot: Vec<&str> = written.boot.files.iter().map(|f| f.path.as_str()).collect();
            assert!(boot.contains(&UNATTEND_FILE) && boot.contains(&"sources/boot.wim"));
            assert!(!boot.contains(&"sources/install.wim"));
            let install: Vec<&str> = written
                .install
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect();
            assert_eq!(install, ["sources/install.wim"]);
            assert_eq!(written.install.files[0].bytes, split.image_bytes());

            let mut again = scratch.open(sector);
            verify_windows(&mut again, &plan_of(layout), &written, &mut |_| {}).unwrap();

            let mut first = vec![0u8; sector as usize];
            File::open(&scratch.0)
                .unwrap()
                .read_exact(&mut first)
                .unwrap();
            assert_eq!((first[446 + 4], first[462 + 4]), (TYPE_FAT32, TYPE_EXFAT));
            assert_eq!(first[446], 0x80, "partition 1 starts the drive");
        }
    }

    #[test]
    fn the_progress_counts_each_byte_of_the_tree_once() {
        let mut events = Vec::new();
        let (scratch, split, written, layout) = written(512, &mut events);
        let total = split.boot_files().1 + b"<unattend/>".len() as u64 + split.image_bytes();
        assert_eq!(
            events[0],
            ProgressEvent::Start {
                stage: Stage::Write,
                total_bytes: Some(total)
            }
        );
        let last = events.len() - 1;
        assert_eq!(
            events[last],
            ProgressEvent::Done {
                stage: Stage::Write,
                bytes_done: total
            }
        );
        assert!(matches!(
            events[last - 1],
            ProgressEvent::Done {
                stage: Stage::Flush,
                ..
            }
        ));

        let mut checked = Vec::new();
        let mut again = scratch.open(512);
        verify_windows(&mut again, &plan_of(layout), &written, &mut |e| {
            checked.push(e)
        })
        .unwrap();
        assert_eq!(written.files().1, total);
        assert_eq!(
            checked.last(),
            Some(&ProgressEvent::Done {
                stage: Stage::Verify,
                bytes_done: total
            })
        );
    }

    #[test]
    fn the_check_finds_a_byte_of_the_install_image_that_changed() {
        let (scratch, _, written, layout) = written(512, &mut Vec::new());
        // The image carries a pattern that no other file holds, so its first
        // bytes find it on the drive.
        let image = pattern(64, 9);
        let bytes = fs::read(&scratch.0).unwrap();
        let at = bytes.windows(64).position(|w| w == image).unwrap() as u64 + 100;
        let mut file = OpenOptions::new().write(true).open(&scratch.0).unwrap();
        file.seek(SeekFrom::Start(at)).unwrap();
        file.write_all(&[bytes[at as usize] ^ 0xFF]).unwrap();
        drop(file);

        let e = verify_windows(
            &mut scratch.open(512),
            &plan_of(layout),
            &written,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(&e, Error::VolumeDiffers { path, .. } if path == "sources/install.wim"),
            "{e}"
        );
    }

    #[test]
    fn the_check_finds_a_table_that_changed() {
        let (scratch, _, written, layout) = written(512, &mut Vec::new());
        let mut file = OpenOptions::new().write(true).open(&scratch.0).unwrap();
        file.seek(SeekFrom::Start(446 + 4)).unwrap();
        file.write_all(&[0x83]).unwrap();
        drop(file);
        let e = verify_windows(
            &mut scratch.open(512),
            &plan_of(layout),
            &written,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.to_string().contains("the partition table"), "{e}");
    }

    #[test]
    fn autounattend_goes_onto_partition_1_with_its_bytes() {
        let (scratch, _, written, _) = written(512, &mut Vec::new());
        let unattend = written
            .boot
            .files
            .iter()
            .find(|f| f.path == TreePath::new(UNATTEND_FILE).unwrap())
            .unwrap();
        assert_eq!(unattend.digest, burnout_core::sha256(b"<unattend/>"));
        drop(scratch);
    }
}
