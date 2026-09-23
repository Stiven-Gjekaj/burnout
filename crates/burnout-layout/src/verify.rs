//! Check a FAT32 volume against the manifest of what went onto it.
//!
//! The check mounts the volume again, with a new cache. A read through the
//! cache that wrote would prove only that the cache holds the files.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};

use burnout_core::{BlockTarget, Digest, Error, Result, Sha256};
use fatfs::{Dir, ReadWriteSeek};

use crate::fat32::with_volume;
use crate::manifest::Manifest;
use burnout_core::TreePath;

/// How much of a file one read takes.
const CHUNK_BYTES: usize = 1024 * 1024;

/// Read each file of `manifest` back from the FAT32 volume that fills
/// `volume`, and compare it with what went in.
///
/// The volume has to hold each directory and each file of the manifest, each
/// file with its size and its digest, and nothing else. The first difference
/// is the error.
pub fn verify_fat32<T: BlockTarget>(volume: T, manifest: &Manifest) -> Result<()> {
    with_volume(volume, |fs| {
        let root = fs.root_dir();
        // What the volume holds, from a walk of its own directories.
        let mut dirs = BTreeSet::new();
        let mut files = BTreeMap::new();
        walk(&root, None, &mut dirs, &mut files)?;

        for dir in &manifest.dirs {
            if !dirs.remove(dir) {
                return Err(differs(dir, "the volume holds no directory of this name"));
            }
        }
        let mut chunk = vec![0u8; CHUNK_BYTES];
        for copied in &manifest.files {
            let path = &copied.path;
            let Some(bytes) = files.remove(path) else {
                return Err(differs(path, "the volume holds no file of this name"));
            };
            if bytes != copied.bytes {
                return Err(differs(
                    path,
                    &format!(
                        "the volume holds {bytes} bytes and the source {}",
                        copied.bytes
                    ),
                ));
            }
            let digest = digest_of(&root, path, &mut chunk)?;
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
    })
}

/// Every directory and every file below `dir`, with the size of each file.
fn walk<IO: ReadWriteSeek>(
    dir: &Dir<'_, IO>,
    above: Option<&TreePath>,
    dirs: &mut BTreeSet<TreePath>,
    files: &mut BTreeMap<TreePath, u64>,
) -> Result<()> {
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let path = match above {
            Some(above) => above.join(&name)?,
            None => TreePath::new(&name)?,
        };
        if entry.is_dir() {
            walk(&entry.to_dir(), Some(&path), dirs, files)?;
            dirs.insert(path);
        } else {
            files.insert(path, entry.len());
        }
    }
    Ok(())
}

/// The digest of a file on the volume.
fn digest_of<IO: ReadWriteSeek>(
    root: &Dir<'_, IO>,
    path: &TreePath,
    chunk: &mut [u8],
) -> Result<Digest> {
    let unreadable = |e: io::Error| differs(path, &format!("it cannot be read back: {e}"));
    let mut file = root.open_file(path.as_str()).map_err(unreadable)?;
    let mut hash = Sha256::new();
    loop {
        match file.read(chunk) {
            Ok(0) => return Ok(hash.finish()),
            Ok(n) => hash.update(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(unreadable(e)),
        }
    }
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
    use crate::testing::{pattern, windows_like, Drive};
    use crate::{copy_to_fat32, CopiedFile};
    use std::io::{Seek, SeekFrom, Write};

    fn copied(sector: u32) -> (Drive, Manifest) {
        let mut d = Drive::formatted(sector);
        let manifest = copy_to_fat32(d.partition(), &windows_like()).unwrap();
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
            verify_fat32(d.partition(), &manifest).unwrap();
        }
    }

    #[test]
    fn one_byte_changed_on_the_drive_is_found() {
        // The change goes to the drive itself, under any cache. The check
        // finds it only if it reads the drive again.
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

        let e = verify_fat32(d.partition(), &manifest);
        assert_eq!(differs_at(e), "efi/boot/bootx64.efi");
    }

    #[test]
    fn a_file_of_another_size_is_found() {
        let (mut d, mut manifest) = copied(512);
        manifest.files[0].bytes += 1;
        let path = manifest.files[0].path.to_string();
        assert_eq!(differs_at(verify_fat32(d.partition(), &manifest)), path);
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
            differs_at(verify_fat32(d.partition(), &manifest)),
            "setup.exe"
        );
    }

    #[test]
    fn a_directory_that_is_not_on_the_volume_is_found() {
        let (mut d, mut manifest) = copied(512);
        manifest.dirs.push(TreePath::new("boot").unwrap());
        assert_eq!(differs_at(verify_fat32(d.partition(), &manifest)), "boot");
    }

    #[test]
    fn an_entry_that_the_source_does_not_hold_is_found() {
        let (mut d, manifest) = copied(512);
        with_volume(d.partition(), |fs| {
            fs.root_dir().create_file("extra.txt")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            differs_at(verify_fat32(d.partition(), &manifest)),
            "extra.txt"
        );
    }
}
