//! Check an exFAT volume against the manifest of what went onto it.
//!
//! The check opens the volume again with a reader of its own, which takes
//! every number from the drive. A read through the writer would prove only
//! that the writer agrees with itself.

use std::collections::{BTreeMap, BTreeSet};

use burnout_core::{BlockTarget, Error, Result, Sha256};

use super::read::{Node, Volume};
use crate::manifest::Manifest;
use burnout_core::TreePath;

/// Read each file of `manifest` back from the exFAT volume that fills
/// `volume`, and compare it with what went in.
///
/// The volume has to be exFAT that the reader accepts: a boot region with
/// its checksum and its copy, an entry set with its checksum and the hash of
/// its name for each entry, and a bitmap that marks each cluster in use and
/// no other. It has to hold each directory and each file of the manifest,
/// each file with its size and its digest, and nothing else. The first
/// difference is the error.
pub fn verify_exfat<T: BlockTarget>(volume: T, manifest: &Manifest) -> Result<()> {
    verify_exfat_with(volume, manifest, &mut |_| {})
}

/// [`verify_exfat`], and `tally` hears the bytes of each piece of a file as
/// it is read back.
pub fn verify_exfat_with<T: BlockTarget>(
    volume: T,
    manifest: &Manifest,
    tally: &mut dyn FnMut(u64),
) -> Result<()> {
    let mut volume = Volume::open(volume)?;
    let nodes = volume.walk()?;
    volume.check_allocation()?;

    let mut dirs = BTreeSet::new();
    let mut files: BTreeMap<TreePath, Node> = BTreeMap::new();
    for node in nodes {
        if node.is_dir {
            dirs.insert(node.path);
        } else {
            files.insert(node.path.clone(), node);
        }
    }

    for dir in &manifest.dirs {
        if !dirs.remove(dir) {
            return Err(differs(dir, "the volume holds no directory of this name"));
        }
    }
    for copied in &manifest.files {
        let path = &copied.path;
        let Some(node) = files.remove(path) else {
            return Err(differs(path, "the volume holds no file of this name"));
        };
        if node.data.length != copied.bytes {
            return Err(differs(
                path,
                &format!(
                    "the volume holds {} bytes and the source {}",
                    node.data.length, copied.bytes
                ),
            ));
        }
        let mut hash = Sha256::new();
        volume.read_file(&node, &mut |piece| {
            hash.update(piece);
            tally(piece.len() as u64);
        })?;
        let digest = hash.finish();
        if digest != copied.digest {
            return Err(differs(
                path,
                &format!("the volume holds {digest} and the source {}", copied.digest),
            ));
        }
    }
    if let Some(extra) = dirs.iter().chain(files.keys()).min() {
        return Err(differs(
            extra,
            "the volume holds it and the source does not",
        ));
    }
    Ok(())
}

fn differs(path: &TreePath, detail: &str) -> Error {
    Error::VolumeDiffers {
        path: path.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exfat::{write_exfat, ExfatOptions};
    use crate::testing::{pattern, windows_like, Drive, MIB};
    use crate::CopiedFile;
    use burnout_core::MemorySource;
    use std::io::{Seek, SeekFrom, Write};

    fn options() -> ExfatOptions<'static> {
        ExfatOptions {
            label: "Install",
            serial: 7,
            first_sector: 0,
        }
    }

    fn copied(sector: u32) -> (Drive, Manifest) {
        let mut d = Drive::new(sector, 64 * MIB);
        let manifest = write_exfat(d.partition(), &windows_like(), &options()).unwrap();
        (d, manifest)
    }

    fn differs_at(result: Result<()>) -> String {
        match result {
            Err(Error::VolumeDiffers { path, .. }) => path,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_copied_tree_passes() {
        for sector in [512, 4096] {
            let (mut d, manifest) = copied(sector);
            verify_exfat(d.partition(), &manifest).unwrap();
        }
    }

    #[test]
    fn one_byte_changed_on_the_drive_is_found() {
        // The change goes to the drive itself. The check finds it only if it
        // reads the drive again.
        let (mut d, manifest) = copied(512);
        let file = pattern(12_289, 2);
        let bytes = d.bytes();
        let at = bytes
            .windows(64)
            .position(|w| w == &file[..64])
            .expect("the file is on the drive") as u64;
        let sector = at / 512 * 512;
        let mut block = bytes[sector as usize..sector as usize + 512].to_vec();
        block[(at - sector) as usize + 100] ^= 0x01;
        d.strict.seek(SeekFrom::Start(sector)).unwrap();
        d.strict.write_all(&block).unwrap();

        let e = verify_exfat(d.partition(), &manifest);
        assert_eq!(differs_at(e), "efi/boot/bootx64.efi");
    }

    #[test]
    fn a_file_of_another_size_is_found() {
        let (mut d, mut manifest) = copied(512);
        manifest.files[0].bytes += 1;
        let path = manifest.files[0].path.to_string();
        assert_eq!(differs_at(verify_exfat(d.partition(), &manifest)), path);
    }

    #[test]
    fn a_file_that_is_not_on_the_volume_is_found() {
        let (mut d, mut manifest) = copied(512);
        let digest = manifest.files[0].digest;
        manifest.files.push(CopiedFile {
            path: TreePath::new("setup.exe").unwrap(),
            bytes: 0,
            digest,
        });
        assert_eq!(
            differs_at(verify_exfat(d.partition(), &manifest)),
            "setup.exe"
        );
    }

    #[test]
    fn a_directory_that_is_not_on_the_volume_is_found() {
        let (mut d, mut manifest) = copied(512);
        manifest.dirs.push(TreePath::new("boot").unwrap());
        assert_eq!(differs_at(verify_exfat(d.partition(), &manifest)), "boot");
    }

    #[test]
    fn an_entry_that_the_source_does_not_hold_is_found() {
        let (_, manifest) = copied(512);
        let mut more = windows_like();
        more.add_file("extra.txt", *b"extra").unwrap();
        let mut d = Drive::new(512, 64 * MIB);
        write_exfat(d.partition(), &more, &options()).unwrap();
        assert_eq!(
            differs_at(verify_exfat(d.partition(), &manifest)),
            "extra.txt"
        );
    }

    #[test]
    fn a_volume_that_is_not_valid_fails_before_any_file_is_read() {
        let (mut d, manifest) = copied(512);
        d.strict.seek(SeekFrom::Start(MIB)).unwrap();
        d.strict.write_all(&[0u8; 512]).unwrap();
        let e = verify_exfat(d.partition(), &manifest).unwrap_err();
        assert!(
            e.to_string().contains("is not the boot region of exFAT"),
            "{e}"
        );
    }

    #[test]
    fn an_empty_tree_makes_a_volume_that_passes() {
        let mut d = Drive::new(4096, 8 * MIB);
        let manifest = write_exfat(d.partition(), &MemorySource::new(), &options()).unwrap();
        assert_eq!(manifest, Manifest::default());
        verify_exfat(d.partition(), &manifest).unwrap();
    }
}
