//! A walk of the tree of UDF, from the root of the file set down.
//!
//! A directory is a file whose data is a list of file identifier
//! descriptors. Each one gives a name, says whether it names a directory,
//! and points at the file entry of what it names.

use std::collections::HashSet;
use std::io::{Read, Seek};

use burnout_core::{Result, TreePath};

use super::entry::{
    read_entry, Entry, Run, BLOCK_DEVICE, CHARACTER_DEVICE, DIRECTORY, FIFO, FILE, LINK, SOCKET,
};
use super::tag::{name, read_tag, FILE_IDENTIFIER, FILE_SET};
use super::text::cs0;
use super::volume::{Address, Volume};
use super::{u16_at, u32_at};
use crate::image::{Image, SECTOR};
use crate::node::{Kind, Node, DEEPEST};

/// The characteristics of a file identifier descriptor. A hidden file is
/// still a part of the tree, because a drive that starts a system needs it.
const IS_DIRECTORY: u8 = 0x02;
const DELETED: u8 = 0x04;
const PARENT: u8 = 0x08;

/// The bytes of a file identifier descriptor before its implementation use
/// and its name.
const FIXED: usize = 38;

/// Every directory, file and link below the root of the file set, a
/// directory before what it holds.
pub(crate) fn walk<R: Read + Seek>(image: &mut Image<R>, volume: &Volume) -> Result<Vec<Node>> {
    let root = root(image, volume)?;
    let entry = read_entry(image, volume, root, "the root directory")?;
    if entry.file_type != DIRECTORY {
        return Err(image.fault(format!(
            "the root of the file set has the file type {}, and not a directory",
            entry.file_type
        )));
    }
    let mut walk = Walk {
        image,
        volume,
        seen: HashSet::new(),
        out: Vec::new(),
    };
    walk.dir(root, &entry, None, 0)?;
    Ok(walk.out)
}

/// The file entry of the root directory, which the file set descriptor
/// gives.
fn root<R: Read + Seek>(image: &mut Image<R>, volume: &Volume) -> Result<Address> {
    let what = "the file set descriptor";
    let fault = |image: &Image<R>, e: String| image.fault(format!("{what}: {e}"));
    let at = volume
        .locate(volume.file_set, SECTOR)
        .map_err(|e| fault(image, e))?;
    let d = image.read_at(at, SECTOR as usize, what)?;
    let tag = read_tag(&d, volume.file_set.block).map_err(|e| fault(image, e))?;
    if tag.id != FILE_SET {
        return Err(fault(
            image,
            format!("block {} holds {}", volume.file_set.block, name(tag.id)),
        ));
    }
    Ok(Address {
        block: u32_at(&d, 404),
        partition: u16_at(&d, 408),
    })
}

struct Walk<'a, R> {
    image: &'a mut Image<R>,
    volume: &'a Volume,
    /// The directories that the walk has gone into, so that a loop is found.
    seen: HashSet<Address>,
    out: Vec<Node>,
}

impl<R: Read + Seek> Walk<'_, R> {
    fn dir(
        &mut self,
        icb: Address,
        entry: &Entry,
        above: Option<&TreePath>,
        depth: usize,
    ) -> Result<()> {
        let at_dir = above.map_or_else(|| "the root directory".to_string(), |p| p.to_string());
        if depth > DEEPEST {
            let e = format!("{at_dir} is more than {DEEPEST} levels deep");
            return Err(self.image.fault(e));
        }
        if !self.seen.insert(icb) {
            let e = format!("{at_dir} holds a directory above it");
            return Err(self.image.fault(e));
        }
        let bytes = self.data(entry, &at_dir)?;
        let mut taken = HashSet::new();
        let mut at = 0;
        while at < bytes.len() {
            let location = block_of(&entry.runs, at as u64);
            let fid = identifier(&bytes[at..], location)
                .map_err(|e| self.image.fault(format!("{at_dir}: {e}")))?;
            at += fid.length;
            if fid.characteristics & (PARENT | DELETED) != 0 {
                continue;
            }
            let name = cs0(&fid.name)
                .and_then(tree_name)
                .map_err(|e| self.image.fault(format!("{at_dir}: {e}")))?;
            let path = match above {
                Some(above) => above.join(&name)?,
                None => TreePath::new(&name)?,
            };
            if !taken.insert(name.clone()) {
                let e = format!("{at_dir} holds the name {name:?} twice");
                return Err(self.image.fault(e));
            }
            let child = read_entry(self.image, self.volume, fid.icb, path.as_str())?;
            let says_dir = fid.characteristics & IS_DIRECTORY != 0;
            if says_dir != (child.file_type == DIRECTORY) {
                let e = format!(
                    "{path}: its identifier and its file entry do not agree that it is a directory"
                );
                return Err(self.image.fault(e));
            }
            match child.file_type {
                DIRECTORY => {
                    self.out.push(Node {
                        path: path.clone(),
                        kind: Kind::Dir,
                        size: 0,
                        extents: Vec::new(),
                    });
                    self.dir(fid.icb, &child, Some(&path), depth + 1)?;
                }
                FILE => self.out.push(Node {
                    path,
                    kind: Kind::File,
                    size: child.size,
                    extents: child.runs.iter().map(|r| r.extent).collect(),
                }),
                LINK => {
                    let data = self.data(&child, path.as_str())?;
                    let target =
                        link_target(&data).map_err(|e| self.image.fault(format!("{path}: {e}")))?;
                    self.out.push(Node {
                        path,
                        kind: Kind::Link(target),
                        size: 0,
                        extents: Vec::new(),
                    });
                }
                // A device, a pipe or a socket is nothing that a drive can
                // hold.
                BLOCK_DEVICE | CHARACTER_DEVICE | FIFO | SOCKET => {}
                other => {
                    let e =
                        format!("{path} has the file type {other}, which Burnout does not read");
                    return Err(self.image.fault(e));
                }
            }
        }
        Ok(())
    }

    /// The bytes of an entry, from each of its runs.
    fn data(&mut self, entry: &Entry, what: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        for run in &entry.runs {
            let length = run.extent.length as usize;
            match run.extent.zero {
                true => bytes.resize(bytes.len() + length, 0),
                false => bytes.extend(self.image.read_at(run.extent.at, length, what)?),
            }
        }
        Ok(bytes)
    }
}

/// The block that holds byte `offset` of the data of an entry, or `None` in
/// a run that was never written.
fn block_of(runs: &[Run], mut offset: u64) -> Option<u32> {
    for run in runs {
        if offset < run.extent.length {
            return run.address.map(|a| a.block + (offset / SECTOR) as u32);
        }
        offset -= run.extent.length;
    }
    None
}

/// One file identifier descriptor.
struct Identifier {
    characteristics: u8,
    /// The name in CS0.
    name: Vec<u8>,
    /// The file entry of what it names.
    icb: Address,
    /// Its bytes, with the padding to a multiple of four.
    length: usize,
}

/// The file identifier descriptor at the start of `bytes`, which starts in
/// block `location`.
fn identifier(bytes: &[u8], location: Option<u32>) -> std::result::Result<Identifier, String> {
    let location =
        location.ok_or("a file identifier descriptor is in blocks that were never written")?;
    let short = || "a file identifier descriptor does not fit the directory".to_string();
    let head = bytes.get(..FIXED).ok_or_else(short)?;
    let (name_length, use_length) = (head[19] as usize, u16_at(head, 36) as usize);
    let length = (FIXED + use_length + name_length).div_ceil(4) * 4;
    let d = bytes.get(..length).ok_or_else(short)?;
    let tag = read_tag(d, location)?;
    if tag.id != FILE_IDENTIFIER {
        return Err(format!(
            "block {location} holds {}, and not a file identifier descriptor",
            name(tag.id)
        ));
    }
    let start = FIXED + use_length;
    Ok(Identifier {
        characteristics: d[18],
        name: d[start..start + name_length].to_vec(),
        icb: Address {
            block: u32_at(d, 24),
            partition: u16_at(d, 28),
        },
        length,
    })
}

/// A name that a tree can hold.
fn tree_name(name: String) -> std::result::Result<String, String> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(format!("the name {name:?} is not one that a tree can hold"));
    }
    Ok(name)
}

/// The path that the data of a link names. Each component has a type, the
/// length of its name, and the name in CS0.
fn link_target(bytes: &[u8]) -> std::result::Result<String, String> {
    let mut path = String::new();
    let mut at = 0;
    while at < bytes.len() {
        let short = || "a component of the link does not fit it".to_string();
        let head = bytes.get(at..at + 4).ok_or_else(short)?;
        let length = head[1] as usize;
        let ident = bytes.get(at + 4..at + 4 + length).ok_or_else(short)?;
        at += 4 + length;
        let part = match head[0] {
            // The root, of the file set or of the system.
            1 | 2 => {
                path = "/".to_string();
                continue;
            }
            3 => "..".to_string(),
            4 => ".".to_string(),
            5 => cs0(ident)?,
            kind => return Err(format!("a component of the link has the type {kind}")),
        };
        if !path.is_empty() && !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(&part);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cs0 as to_cs0, retag, udf, Ads, Item, Udf, UDF_PARTITION};
    use crate::udf::find_volume;
    use std::io::Cursor;

    fn tree(image: &[u8]) -> Result<Vec<Node>> {
        let mut i = Image::new(Cursor::new(image.to_vec()), "test.iso").unwrap();
        let volume = find_volume(&mut i).unwrap().unwrap();
        walk(&mut i, &volume)
    }

    fn paths(nodes: &[Node]) -> Vec<&str> {
        nodes.iter().map(|n| n.path.as_str()).collect()
    }

    fn read(image: &[u8], node: &Node) -> Vec<u8> {
        let mut bytes = Vec::new();
        for e in &node.extents {
            match e.zero {
                true => bytes.resize(bytes.len() + e.length as usize, 0),
                false => bytes.extend_from_slice(&image[e.at as usize..(e.at + e.length) as usize]),
            }
        }
        bytes
    }

    /// Change the identifier of `name` in an image that the builder wrote,
    /// and set its tag again.
    fn patch_identifier(image: &mut [u8], name: &str, change: impl FnOnce(&mut [u8])) {
        let needle = to_cs0(name);
        let partition = UDF_PARTITION as usize * 2048;
        let found = image[partition..]
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap();
        let at = partition + found - FIXED;
        let length = (FIXED + needle.len()).div_ceil(4) * 4;
        change(&mut image[at..at + length]);
        retag(&mut image[at..at + length]);
    }

    /// Change the file entry at a block of the partition, and set its tag
    /// again. The builder puts the entry of item `n` at block `n + 2`.
    fn patch_entry(image: &mut [u8], block: u32, change: impl FnOnce(&mut [u8])) {
        let at = (UDF_PARTITION + block) as usize * 2048;
        change(&mut image[at..at + 2048]);
        retag(&mut image[at..at + 2048]);
    }

    const BIG: &[u8] = &[0x42; 5000];

    const ITEMS: &[Item] = &[
        Item::Dir("sources"),
        Item::File("sources/install.wim", BIG),
        Item::Dir("sources/empty"),
        Item::File("\u{3053}\u{3093}.txt", b"kon"),
        Item::File("setup.exe", b"MZ"),
        Item::File("empty.txt", b""),
    ];

    #[test]
    fn the_tree_gives_each_entry_with_its_name_and_its_data() {
        let image = udf(ITEMS, &Udf::default());
        let nodes = tree(&image).unwrap();
        assert_eq!(
            paths(&nodes),
            [
                "sources",
                "sources/install.wim",
                "sources/empty",
                "\u{3053}\u{3093}.txt",
                "setup.exe",
                "empty.txt"
            ]
        );
        assert_eq!(nodes[0].kind, Kind::Dir);
        assert_eq!((nodes[1].kind.clone(), nodes[1].size), (Kind::File, 5000));
        assert_eq!(read(&image, &nodes[1]), BIG);
        assert_eq!(read(&image, &nodes[3]), b"kon");
        assert!(nodes[5].extents.is_empty());
    }

    #[test]
    fn each_form_of_entry_and_of_descriptor_gives_the_same_tree() {
        let expected = tree(&udf(ITEMS, &Udf::default())).unwrap();
        for (extended, ads) in [
            (false, Ads::Long),
            (true, Ads::Short),
            (true, Ads::Embedded),
            (false, Ads::Embedded),
        ] {
            let options = Udf {
                extended,
                ads,
                ..Udf::default()
            };
            let image = udf(ITEMS, &options);
            let nodes = tree(&image).unwrap();
            assert_eq!(paths(&nodes), paths(&expected), "{options:?}");
            for (node, want) in nodes.iter().zip(ITEMS) {
                if let Item::File(_, bytes) = want {
                    assert_eq!(read(&image, node), *bytes, "{options:?} {}", node.path);
                }
            }
        }
    }

    #[test]
    fn a_directory_of_many_identifiers_spans_its_blocks() {
        let names: Vec<String> = (0..200).map(|n| format!("dir/file {n:03}.txt")).collect();
        let mut items = vec![Item::Dir("dir")];
        items.extend(names.iter().map(|n| Item::File(n, b"z")));
        let nodes = tree(&udf(&items, &Udf::default())).unwrap();
        assert_eq!(nodes.len(), 201);
        assert_eq!(nodes[200].path.as_str(), "dir/file 199.txt");
    }

    #[test]
    fn a_link_is_a_node_with_its_target() {
        let items = [
            Item::Dir("boot"),
            Item::Link("boot/latest", "../setup.exe"),
            Item::File("setup.exe", b"MZ"),
            Item::Link("etc", "/usr/etc"),
        ];
        let nodes = tree(&udf(&items, &Udf::default())).unwrap();
        assert_eq!(nodes[1].kind, Kind::Link("../setup.exe".into()));
        assert_eq!(nodes[3].kind, Kind::Link("/usr/etc".into()));
    }

    const TWO: &[Item] = &[
        Item::Dir("docs"),
        Item::File("docs/x.txt", b"x"),
        Item::File("a.txt", b"a"),
        Item::File("b.txt", b"b"),
    ];

    #[test]
    fn a_deleted_file_is_not_a_part_of_the_tree_and_a_hidden_one_is() {
        let mut image = udf(TWO, &Udf::default());
        patch_identifier(&mut image, "a.txt", |f| f[18] = DELETED);
        patch_identifier(&mut image, "b.txt", |f| f[18] = 0x01);
        assert_eq!(
            paths(&tree(&image).unwrap()),
            ["docs", "docs/x.txt", "b.txt"]
        );
    }

    #[test]
    fn an_identifier_at_the_wrong_block_is_refused() {
        let mut image = udf(TWO, &Udf::default());
        patch_identifier(&mut image, "b.txt", |f| f[12] += 1);
        let e = tree(&image).unwrap_err().to_string();
        assert!(
            e.contains("the root directory: a file identifier descriptor says"),
            "{e}"
        );
    }

    #[test]
    fn a_name_twice_in_one_directory_is_refused() {
        let mut image = udf(TWO, &Udf::default());
        patch_identifier(&mut image, "b.txt", |f| f[FIXED + 1] = b'a');
        let e = tree(&image).unwrap_err().to_string();
        assert!(e.contains("holds the name \"a.txt\" twice"), "{e}");
    }

    #[test]
    fn a_name_with_a_slash_is_refused() {
        let mut image = udf(TWO, &Udf::default());
        patch_identifier(&mut image, "b.txt", |f| f[FIXED + 2] = b'/');
        let e = tree(&image).unwrap_err().to_string();
        assert!(
            e.contains("\"b/txt\" is not one that a tree can hold"),
            "{e}"
        );
    }

    #[test]
    fn a_directory_that_holds_the_root_is_refused() {
        let mut image = udf(TWO, &Udf::default());
        // The root has its entry at block 1.
        patch_identifier(&mut image, "docs", |f| {
            f[24..28].copy_from_slice(&1u32.to_le_bytes())
        });
        let e = tree(&image).unwrap_err().to_string();
        assert!(e.contains("docs holds a directory above it"), "{e}");
    }

    #[test]
    fn an_identifier_and_an_entry_that_do_not_agree_are_refused() {
        let mut image = udf(TWO, &Udf::default());
        patch_identifier(&mut image, "docs", |f| f[18] = 0);
        let e = tree(&image).unwrap_err().to_string();
        let disagree = "docs: its identifier and its file entry do not agree";
        assert!(e.contains(disagree), "{e}");
    }

    #[test]
    fn a_device_is_not_a_part_of_the_tree() {
        let mut image = udf(TWO, &Udf::default());
        // Item 2, a.txt, has its entry at block 4.
        patch_entry(&mut image, 4, |fe| fe[27] = CHARACTER_DEVICE);
        assert_eq!(
            paths(&tree(&image).unwrap()),
            ["docs", "docs/x.txt", "b.txt"]
        );
    }

    #[test]
    fn a_file_type_that_burnout_does_not_read_is_refused() {
        let mut image = udf(TWO, &Udf::default());
        patch_entry(&mut image, 4, |fe| fe[27] = 250);
        let e = tree(&image).unwrap_err().to_string();
        assert!(e.contains("a.txt has the file type 250"), "{e}");
    }

    #[test]
    fn an_image_without_its_file_set_descriptor_is_refused() {
        let mut image = udf(TWO, &Udf::default());
        let at = UDF_PARTITION as usize * 2048;
        image[at..at + 2048].fill(0);
        let e = tree(&image).unwrap_err().to_string();
        assert!(
            e.contains("the file set descriptor: the tag at block 0 has version 0"),
            "{e}"
        );
    }
}
