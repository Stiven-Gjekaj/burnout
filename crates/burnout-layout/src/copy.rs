//! Copy a tree of files onto a FAT32 volume, and keep the digest of each
//! file.
//!
//! The digests are what a later check compares against. Raw mode checks one
//! digest of the whole drive. Windows mode writes files, so it checks one
//! digest for each file.

use std::io::{self, Read, Write};

use burnout_core::{BlockTarget, Digest, Error, Result, Sha256};
use fatfs::{Dir, ReadWriteSeek};

use crate::fat32::with_volume;
use crate::tree::{Entry, FileSource, TreePath};

/// The largest file that FAT32 holds: 4 GiB less one byte.
pub const MAX_FAT32_FILE_BYTES: u64 = u32::MAX as u64;

/// How much of a file one read takes.
const CHUNK_BYTES: usize = 1024 * 1024;

/// A file that went onto a volume, and the digest of the bytes that went in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopiedFile {
    pub path: TreePath,
    pub bytes: u64,
    pub digest: Digest,
}

/// Everything that a copy put onto a volume, in the order it went on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub dirs: Vec<TreePath>,
    pub files: Vec<CopiedFile>,
}

/// Copy every directory and every file of `source` onto the FAT32 volume
/// that fills `volume`.
///
/// The digest of each file is of the bytes that came from the source, taken
/// on their way onto the volume.
///
/// A file larger than [`MAX_FAT32_FILE_BYTES`] is refused before a byte is
/// written. Cutting it short would give a copy that is not the file.
///
/// FAT32 does not tell `EFI` from `efi`, and it does not tell a long name
/// from the short name of another entry. A second name for an entry that is
/// already there is refused. `fatfs` would otherwise open the entry that is
/// there and write into it.
pub fn copy_to_fat32<T, S>(volume: T, source: &S) -> Result<Manifest>
where
    T: BlockTarget,
    S: FileSource + ?Sized,
{
    let entries = source.entries()?;
    for entry in &entries {
        if let Entry::File(path, bytes) = entry {
            if *bytes > MAX_FAT32_FILE_BYTES {
                return Err(Error::FileTooLarge {
                    path: path.to_string(),
                    bytes: *bytes,
                    file_system: "FAT32",
                    limit: MAX_FAT32_FILE_BYTES,
                });
            }
        }
    }

    with_volume(volume, |fs| {
        let root = fs.root_dir();
        let mut manifest = Manifest::default();
        let mut chunk = vec![0u8; CHUNK_BYTES];
        for entry in &entries {
            let path = entry.path();
            let parent = match path.parent() {
                Some(parent) => Some(
                    root.open_dir(parent.as_str())
                        .map_err(|e| cannot_copy(path, e))?,
                ),
                None => None,
            };
            let dir = parent.as_ref().unwrap_or(&root);
            refuse_second_name(dir, path)?;

            match entry {
                Entry::Dir(path) => {
                    dir.create_dir(path.name())
                        .map_err(|e| cannot_copy(path, e))?;
                    manifest.dirs.push(path.clone());
                }
                Entry::File(path, bytes) => {
                    let digest = copy_file(dir, source, path, *bytes, &mut chunk)?;
                    manifest.files.push(CopiedFile {
                        path: path.clone(),
                        bytes: *bytes,
                        digest,
                    });
                }
            }
        }
        Ok(manifest)
    })
}

/// Refuse `path` when its directory already holds an entry that FAT32 does
/// not tell apart from it.
///
/// This is the rule that `fatfs` uses to find an entry by name: the long
/// name or the short name, in upper case.
fn refuse_second_name<IO: ReadWriteSeek>(dir: &Dir<'_, IO>, path: &TreePath) -> Result<()> {
    let wanted = path.name().to_uppercase();
    for entry in dir.iter() {
        let entry = entry.map_err(|e| cannot_copy(path, e))?;
        let long = entry.file_name();
        if long.to_uppercase() == wanted || entry.short_file_name().to_uppercase() == wanted {
            let first = match path.parent() {
                Some(parent) => format!("{parent}/{long}"),
                None => long,
            };
            return Err(Error::CannotCopy {
                path: path.to_string(),
                detail: format!("FAT32 does not tell its name apart from {first}"),
            });
        }
    }
    Ok(())
}

/// Copy one file, and give the digest of the bytes that went in.
fn copy_file<IO, S>(
    dir: &Dir<'_, IO>,
    source: &S,
    path: &TreePath,
    bytes: u64,
    chunk: &mut [u8],
) -> Result<Digest>
where
    IO: ReadWriteSeek,
    S: FileSource + ?Sized,
{
    let mut reader = source.open(path)?;
    let mut file = dir
        .create_file(path.name())
        .map_err(|e| cannot_copy(path, e))?;
    let mut hash = Sha256::new();
    let mut done: u64 = 0;
    while done <= bytes {
        let n = match reader.read(chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(Error::Source {
                    path: path.to_string(),
                    detail: e.to_string(),
                })
            }
        };
        done += n as u64;
        if done <= bytes {
            hash.update(&chunk[..n]);
            file.write_all(&chunk[..n])
                .map_err(|e| cannot_copy(path, e))?;
        }
    }
    if done != bytes {
        let gave = if done > bytes {
            format!("more than {bytes}")
        } else {
            done.to_string()
        };
        return Err(Error::Source {
            path: path.to_string(),
            detail: format!("the tree gave its size as {bytes} bytes, and the file gave {gave}"),
        });
    }
    file.flush().map_err(|e| cannot_copy(path, e))?;
    Ok(hash.finish())
}

fn cannot_copy(path: &TreePath, e: io::Error) -> Error {
    Error::CannotCopy {
        path: path.to_string(),
        detail: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fat32::with_volume;
    use crate::testing::Drive;
    use crate::tree::MemorySource;
    use burnout_core::sha256;

    /// Bytes that differ from one offset to the next, so a block in the
    /// wrong place cannot pass for the right one.
    fn pattern(length: usize, seed: u8) -> Vec<u8> {
        (0..length)
            .map(|i| {
                (i as u8)
                    .wrapping_mul(31)
                    .wrapping_add(seed ^ (i >> 9) as u8)
            })
            .collect()
    }

    /// A tree with a little of everything that a Windows image has.
    fn windows_like() -> MemorySource {
        let mut tree = MemorySource::new();
        tree.add_file("bootmgr", pattern(4000, 1)).unwrap();
        tree.add_file("efi/boot/bootx64.efi", pattern(12_289, 2))
            .unwrap();
        tree.add_file("efi/microsoft/boot/BCD", pattern(3 * 4096, 3))
            .unwrap();
        tree.add_file("sources/boot.wim", pattern(3 * 1024 * 1024 + 7, 4))
            .unwrap();
        tree.add_file("autorun.inf", *b"").unwrap();
        tree.add_file("A long name with spaces.txt", pattern(10, 5))
            .unwrap();
        tree.add_dir("support/logging").unwrap();
        tree
    }

    fn read_back<T: BlockTarget>(volume: T, path: &str) -> Vec<u8> {
        with_volume(volume, |fs| {
            let mut bytes = Vec::new();
            fs.root_dir().open_file(path)?.read_to_end(&mut bytes)?;
            Ok(bytes)
        })
        .unwrap()
    }

    #[test]
    fn every_file_goes_on_with_the_digest_of_its_bytes() {
        let tree = windows_like();
        for sector in [512, 4096] {
            let mut d = Drive::formatted(sector);
            let manifest = copy_to_fat32(d.partition(), &tree).unwrap();

            assert_eq!(manifest.files.len(), 6);
            for copied in &manifest.files {
                let mut want = Vec::new();
                tree.open(&copied.path)
                    .unwrap()
                    .read_to_end(&mut want)
                    .unwrap();
                assert_eq!(copied.digest, sha256(&want), "{}", copied.path);
                assert_eq!(copied.bytes, want.len() as u64);
                assert_eq!(read_back(d.partition(), copied.path.as_str()), want);
            }
        }
    }

    #[test]
    fn every_directory_goes_on_even_when_it_is_empty() {
        let mut d = Drive::formatted(512);
        let manifest = copy_to_fat32(d.partition(), &windows_like()).unwrap();
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
        with_volume(d.partition(), |fs| {
            fs.root_dir().open_dir("support/logging")?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn the_same_tree_makes_the_same_volume() {
        // Every timestamp is the FAT epoch and the order of the tree is
        // fixed, so two copies agree to the byte.
        let mut first = Drive::formatted(512);
        let mut second = Drive::formatted(512);
        copy_to_fat32(first.partition(), &windows_like()).unwrap();
        copy_to_fat32(second.partition(), &windows_like()).unwrap();
        assert!(first.bytes() == second.bytes());
    }

    /// A tree of one file that says it holds `claimed` bytes and gives
    /// `given`.
    struct Claims {
        claimed: u64,
        given: u64,
    }

    impl FileSource for Claims {
        fn entries(&self) -> Result<Vec<Entry>> {
            Ok(vec![Entry::File(
                TreePath::new("install.wim")?,
                self.claimed,
            )])
        }

        fn open(&self, _path: &TreePath) -> Result<Box<dyn Read + '_>> {
            Ok(Box::new(io::repeat(0x5A).take(self.given)))
        }
    }

    #[test]
    fn a_file_too_large_for_fat32_is_refused_before_anything_is_written() {
        let mut d = Drive::formatted(512);
        let before = d.bytes().to_vec();
        let big = Claims {
            claimed: MAX_FAT32_FILE_BYTES + 1,
            given: 0,
        };
        match copy_to_fat32(d.partition(), &big) {
            Err(Error::FileTooLarge { path, bytes, .. }) => {
                assert_eq!(path, "install.wim");
                assert_eq!(bytes, MAX_FAT32_FILE_BYTES + 1);
            }
            other => panic!("{other:?}"),
        }
        assert!(d.bytes() == before.as_slice(), "the volume is untouched");
    }

    #[test]
    fn a_file_that_gives_more_or_fewer_bytes_than_it_said_is_refused() {
        for (claimed, given) in [(10, 20), (10, 5)] {
            let mut d = Drive::formatted(512);
            let e = copy_to_fat32(d.partition(), &Claims { claimed, given }).unwrap_err();
            assert!(matches!(e, Error::Source { .. }), "{e}");
            assert!(e.to_string().contains("install.wim"), "{e}");
        }
    }

    #[test]
    fn names_that_differ_only_in_case_are_refused() {
        let mut tree = MemorySource::new();
        tree.add_file("EFI/a", *b"a").unwrap();
        tree.add_file("efi/b", *b"b").unwrap();
        let mut d = Drive::formatted(512);
        let e = copy_to_fat32(d.partition(), &tree).unwrap_err();
        assert_eq!(
            e.to_string(),
            "cannot copy efi onto the volume: FAT32 does not tell its name apart from EFI"
        );
    }

    #[test]
    fn a_name_that_is_the_short_name_of_another_entry_is_refused() {
        // The long name comes first and gets the short name AAAAAA~1.TXT.
        // Without the check, the second file would be written into the first.
        let mut tree = MemorySource::new();
        tree.add_file("aaaaaaaaaa.txt", *b"first").unwrap();
        tree.add_file("aaaaaa~1.txt", *b"second").unwrap();
        let mut d = Drive::formatted(512);
        let e = copy_to_fat32(d.partition(), &tree).unwrap_err();
        assert!(e.to_string().contains("aaaaaaaaaa.txt"), "{e}");
    }

    #[test]
    fn a_name_that_fat32_cannot_hold_is_refused_with_its_path() {
        let mut tree = MemorySource::new();
        tree.add_file("efi/a:b", *b"x").unwrap();
        let mut d = Drive::formatted(512);
        match copy_to_fat32(d.partition(), &tree) {
            Err(Error::CannotCopy { path, .. }) => assert_eq!(path, "efi/a:b"),
            other => panic!("{other:?}"),
        }
    }
}
