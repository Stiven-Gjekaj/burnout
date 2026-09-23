//! An image as a tree of files, from the file system that holds its tree.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Seek};

use burnout_core::{Entry, Error, FileSource, Result, TreePath};

use crate::image::Image;
use crate::iso9660::{self, detect_rock_ridge, read_descriptors, Names};
use crate::node::{Extent, Kind};
use crate::udf;

/// The file system that gives the tree of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileSystem {
    /// UDF, which a Windows ISO keeps its files in.
    Udf,
    /// ISO 9660 with the names and the links of Rock Ridge, as a Linux ISO
    /// has it.
    RockRidge,
    /// ISO 9660 with the long names of Joliet.
    Joliet,
    /// ISO 9660 with its short names only.
    Iso9660,
}

impl fmt::Display for FileSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileSystem::Udf => "UDF",
            FileSystem::RockRidge => "ISO 9660 with Rock Ridge",
            FileSystem::Joliet => "ISO 9660 with Joliet",
            FileSystem::Iso9660 => "ISO 9660",
        })
    }
}

/// A link of the tree. A copy lists it, and does not follow it, because the
/// file systems of a drive cannot hold it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub path: TreePath,
    /// The path that the link names, as the image records it.
    pub target: String,
}

/// An image, as a tree of files.
///
/// The whole tree is read once, when the source is made. The bytes of a
/// file are read from the image when the file is read.
pub struct IsoSource<R> {
    image: RefCell<Image<R>>,
    file_system: FileSystem,
    label: String,
    entries: Vec<Entry>,
    files: BTreeMap<TreePath, Vec<Extent>>,
    links: Vec<Link>,
}

impl<R: Read + Seek> IsoSource<R> {
    /// The tree of an image, or `None` when the image holds neither UDF nor
    /// ISO 9660. `name` names the image in each error.
    ///
    /// UDF comes first, because the ISO 9660 side of a Windows ISO holds
    /// only a note. Then Rock Ridge, then Joliet, then the short names.
    pub fn new(reader: R, name: &str) -> Result<Option<Self>> {
        let mut image = Image::new(reader, name)?;
        let (file_system, label, nodes) = if let Some(volume) = udf::find_volume(&mut image)? {
            let nodes = udf::walk(&mut image, &volume)?;
            (FileSystem::Udf, volume.label, nodes)
        } else if let Some(d) = read_descriptors(&mut image)? {
            let primary = d.primary.root;
            match (detect_rock_ridge(&mut image, primary)?, d.joliet) {
                (Some(skip), _) => {
                    let nodes = iso9660::walk(&mut image, primary, Names::RockRidge { skip })?;
                    (FileSystem::RockRidge, d.primary.label, nodes)
                }
                (None, Some(joliet)) => {
                    let nodes = iso9660::walk(&mut image, joliet.root, Names::Joliet)?;
                    (FileSystem::Joliet, joliet.label, nodes)
                }
                (None, None) => {
                    let nodes = iso9660::walk(&mut image, primary, Names::Plain)?;
                    (FileSystem::Iso9660, d.primary.label, nodes)
                }
            }
        } else {
            return Ok(None);
        };

        let mut entries = Vec::new();
        let mut files = BTreeMap::new();
        let mut links = Vec::new();
        for node in nodes {
            match node.kind {
                Kind::Dir => entries.push(Entry::Dir(node.path)),
                Kind::File => {
                    entries.push(Entry::File(node.path.clone(), node.size));
                    files.insert(node.path, node.extents);
                }
                Kind::Link(target) => links.push(Link {
                    path: node.path,
                    target,
                }),
            }
        }
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        links.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Some(IsoSource {
            image: RefCell::new(image),
            file_system,
            label,
            entries,
            files,
            links,
        }))
    }

    pub fn file_system(&self) -> FileSystem {
        self.file_system
    }

    /// The name of the volume, as the file system of the tree gives it.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The links of the tree, in the order of their paths.
    pub fn links(&self) -> &[Link] {
        &self.links
    }
}

impl<R: Read + Seek> FileSource for IsoSource<R> {
    fn entries(&self) -> Result<Vec<Entry>> {
        Ok(self.entries.clone())
    }

    fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>> {
        let (path, extents) = self
            .files
            .get_key_value(path)
            .ok_or_else(|| Error::Source {
                path: path.to_string(),
                detail: "the tree holds no file with this name".to_string(),
            })?;
        Ok(Box::new(Extents {
            image: &self.image,
            extents,
            what: path.as_str(),
            index: 0,
            offset: 0,
        }))
    }
}

/// The bytes of one file, from its extents in the image.
struct Extents<'a, R> {
    image: &'a RefCell<Image<R>>,
    extents: &'a [Extent],
    /// The file, for an error.
    what: &'a str,
    /// The extent that the next read starts in, and the bytes of it that
    /// are read already.
    index: usize,
    offset: u64,
}

impl<R: Read + Seek> Read for Extents<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while let Some(extent) = self.extents.get(self.index) {
            let left = extent.length - self.offset;
            if left == 0 {
                self.index += 1;
                self.offset = 0;
                continue;
            }
            let n = left.min(buf.len() as u64) as usize;
            let part = &mut buf[..n];
            if extent.zero {
                part.fill(0);
            } else {
                let at = extent.at + self.offset;
                let mut image = self.image.borrow_mut();
                image.read_into(at, part, self.what).map_err(to_io)?;
            }
            self.offset += part.len() as u64;
            return Ok(part.len());
        }
        Ok(0)
    }
}

fn to_io(e: Error) -> io::Error {
    match e {
        Error::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{bridge, iso9660, Iso, Item};
    use std::io::Cursor;

    type Source = IsoSource<Cursor<Vec<u8>>>;

    fn source(image: Vec<u8>) -> Source {
        IsoSource::new(Cursor::new(image), "test.iso")
            .unwrap()
            .unwrap()
    }

    fn read_all(source: &Source, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut file = source.open(&TreePath::new(path).unwrap()).unwrap();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn dir(path: &str) -> Entry {
        Entry::Dir(TreePath::new(path).unwrap())
    }

    fn file(path: &str, bytes: u64) -> Entry {
        Entry::File(TreePath::new(path).unwrap(), bytes)
    }

    const BIG: &[u8] = &[0x57; 5000];

    #[test]
    fn a_windows_iso_gives_its_udf_tree_and_not_its_note() {
        let note = [Item::File("README.TXT", b"use a reader of UDF")];
        let items = [
            Item::Dir("sources"),
            Item::File("sources/install.wim", BIG),
            Item::File("setup.exe", b"MZ"),
        ];
        let s = source(bridge(&note, &items));
        assert_eq!(s.file_system(), FileSystem::Udf);
        assert_eq!(s.label(), "TEST");
        assert_eq!(
            s.entries().unwrap(),
            [
                file("setup.exe", 2),
                dir("sources"),
                file("sources/install.wim", 5000)
            ]
        );
        assert_eq!(read_all(&s, "sources/install.wim"), BIG);
    }

    #[test]
    fn a_linux_iso_gives_its_rock_ridge_tree_with_its_links_apart() {
        let items = [
            Item::Dir("boot"),
            Item::File("boot/grub.cfg", b"menu"),
            Item::Link("ubuntu", "."),
            Item::File("md5sum.txt", b"sums"),
        ];
        let options = Iso {
            joliet: true,
            rock_ridge: true,
            ..Iso::default()
        };
        let s = source(iso9660(&items, options));
        assert_eq!(s.file_system(), FileSystem::RockRidge);
        assert_eq!(
            s.entries().unwrap(),
            [dir("boot"), file("boot/grub.cfg", 4), file("md5sum.txt", 4)]
        );
        let link = Link {
            path: TreePath::new("ubuntu").unwrap(),
            target: ".".to_string(),
        };
        assert_eq!(s.links(), [link]);
        assert_eq!(read_all(&s, "boot/grub.cfg"), b"menu");
    }

    #[test]
    fn joliet_comes_before_the_short_names() {
        let items = [Item::File("A long name.txt", b"x")];
        let joliet = Iso {
            joliet: true,
            ..Iso::default()
        };
        let s = source(iso9660(&items, joliet));
        assert_eq!(s.file_system(), FileSystem::Joliet);
        assert_eq!(s.entries().unwrap(), [file("A long name.txt", 1)]);
        let s = source(iso9660(&[Item::File("readme.txt", b"x")], Iso::default()));
        assert_eq!(s.file_system(), FileSystem::Iso9660);
        assert_eq!(s.entries().unwrap(), [file("README.TXT", 1)]);
    }

    #[test]
    fn an_image_that_holds_neither_file_system_gives_none() {
        for image in [vec![0u8; 400 * 2048], vec![0u8; 100]] {
            let found = IsoSource::new(Cursor::new(image), "disk.img").unwrap();
            assert!(found.is_none());
        }
    }

    #[test]
    fn a_file_reads_across_its_extents_and_its_runs_of_zeros() {
        let image: Vec<u8> = (0..=255).collect();
        let run = |at, length, zero| Extent { at, length, zero };
        let extents = [
            run(100, 10, false),
            run(0, 5, true),
            run(0, 0, false),
            run(3, 4, false),
        ];
        let expected = [&image[100..110], &[0; 5], &image[3..7]].concat();
        let cell = RefCell::new(Image::new(Cursor::new(image.clone()), "t").unwrap());
        for size in [1, 3, 7, 64] {
            let mut reader = Extents {
                image: &cell,
                extents: &extents,
                what: "a.txt",
                index: 0,
                offset: 0,
            };
            let (mut got, mut buf) = (Vec::new(), vec![0; size]);
            loop {
                let n = reader.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            assert_eq!(got, expected, "reads of {size} bytes");
        }
    }

    #[test]
    fn open_refuses_a_directory_and_a_path_that_is_not_there() {
        let items = [Item::Dir("boot"), Item::File("boot/a.cfg", b"a")];
        let s = source(iso9660(&items, Iso::default()));
        for path in ["BOOT", "NOTHING.TXT"] {
            let e = s.open(&TreePath::new(path).unwrap()).err().unwrap();
            assert!(
                e.to_string().contains("holds no file with this name"),
                "{e}"
            );
        }
    }
}
