//! A walk of one tree of ISO 9660, from its root down.

use std::collections::HashSet;
use std::io::{Read, Seek};

use burnout_core::{Result, TreePath};

use super::descriptors::Root;
use super::directory::{joliet_name, plain_name, records, Record, ASSOCIATED, MULTI_EXTENT};
use crate::image::{Image, SECTOR};
use crate::node::{Extent, Kind, Node};

/// The deepest directory that the walk goes into. ISO 9660 allows eight
/// levels, and Rock Ridge more. A tree deeper than this is a loop or an
/// attack, and not an operating system.
const DEEPEST: usize = 64;

/// Which names a tree keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Names {
    Plain,
    Joliet,
}

/// Every directory and every file below the root, a directory before what
/// it holds.
pub(crate) fn walk<R: Read + Seek>(
    image: &mut Image<R>,
    root: Root,
    names: Names,
) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk_dir(image, root, None, names, &mut seen, &mut out, 0)?;
    Ok(out)
}

fn walk_dir<R: Read + Seek>(
    image: &mut Image<R>,
    dir: Root,
    above: Option<&TreePath>,
    names: Names,
    seen: &mut HashSet<u32>,
    out: &mut Vec<Node>,
    depth: usize,
) -> Result<()> {
    let at_dir = above.map_or_else(|| "the root directory".to_string(), |p| p.to_string());
    if depth > DEEPEST {
        return Err(image.fault(format!("{at_dir} is more than {DEEPEST} levels deep")));
    }
    if !seen.insert(dir.extent) {
        return Err(image.fault(format!("{at_dir} holds a directory above it")));
    }
    let length = (dir.length as usize).div_ceil(SECTOR as usize) * SECTOR as usize;
    let bytes = image.read_at(dir.extent as u64 * SECTOR, length, &at_dir)?;
    let records = records(&bytes).map_err(|e| image.fault(format!("{at_dir}: {e}")))?;

    let mut taken = HashSet::new();
    let mut pending: Option<Node> = None;
    for r in records {
        if r.is_self() || r.is_parent() || r.flags & ASSOCIATED != 0 {
            continue;
        }
        let name = match names {
            Names::Plain => plain_name(&r.id),
            Names::Joliet => joliet_name(&r.id),
        }
        .map_err(|e| image.fault(format!("{at_dir}: {e}")))?;
        let path = match above {
            Some(above) => above.join(&name)?,
            None => TreePath::new(&name)?,
        };

        // A file of more than one extent is a run of records with one name.
        // Each record but the last carries the flag.
        if let Some(node) = pending.as_mut() {
            if node.path != path {
                return Err(image.fault(format!("{}: its last extent is missing", node.path)));
            }
            push_extent(image, node, &r)?;
            if r.flags & MULTI_EXTENT == 0 {
                out.extend(pending.take());
            }
            continue;
        }
        if !taken.insert(name.clone()) {
            return Err(image.fault(format!("{at_dir} holds the name {name:?} twice")));
        }
        if r.is_dir() {
            out.push(Node {
                path: path.clone(),
                kind: Kind::Dir,
                size: 0,
                extents: Vec::new(),
            });
            let inner = Root {
                extent: r.extent,
                length: r.length,
            };
            walk_dir(image, inner, Some(&path), names, seen, out, depth + 1)?;
            continue;
        }
        let mut node = Node {
            path,
            kind: Kind::File,
            size: 0,
            extents: Vec::new(),
        };
        push_extent(image, &mut node, &r)?;
        if r.flags & MULTI_EXTENT != 0 {
            pending = Some(node);
        } else {
            out.push(node);
        }
    }
    if let Some(node) = pending {
        return Err(image.fault(format!("{}: its last extent is missing", node.path)));
    }
    Ok(())
}

/// Add the extent of one record to a file.
fn push_extent<R: Read + Seek>(image: &Image<R>, node: &mut Node, r: &Record) -> Result<()> {
    let length = r.length as u64;
    if length == 0 {
        return Ok(());
    }
    let at = r.extent as u64 * SECTOR;
    if at + length > image.length() {
        return Err(image.fault(format!("{} runs past the end of the image", node.path)));
    }
    node.size += length;
    node.extents.push(Extent {
        at,
        length,
        zero: false,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::read_descriptors;
    use crate::testing::{iso9660, record_bytes, Iso, Item};
    use std::io::Cursor;

    fn tree(image: Vec<u8>, joliet: bool) -> Result<Vec<Node>> {
        let mut i = Image::new(Cursor::new(image), "test.iso").unwrap();
        let d = read_descriptors(&mut i).unwrap().unwrap();
        match joliet {
            true => walk(&mut i, d.joliet.unwrap().root, Names::Joliet),
            false => walk(&mut i, d.primary.root, Names::Plain),
        }
    }

    fn paths(nodes: &[Node]) -> Vec<&str> {
        nodes.iter().map(|n| n.path.as_str()).collect()
    }

    fn read(image: &[u8], node: &Node) -> Vec<u8> {
        node.extents
            .iter()
            .flat_map(|e| image[e.at as usize..(e.at + e.length) as usize].to_vec())
            .collect()
    }

    const ITEMS: &[Item] = &[
        Item::Dir("boot"),
        Item::File("boot/grub.cfg", b"menuentry"),
        Item::Dir("boot/empty"),
        Item::File("md5sum.txt", b"abc  file"),
        Item::File("empty.txt", b""),
    ];

    #[test]
    fn the_primary_tree_gives_each_entry_with_its_plain_name() {
        let image = iso9660(ITEMS, Iso::default());
        let nodes = tree(image.clone(), false).unwrap();
        assert_eq!(
            paths(&nodes),
            [
                "BOOT",
                "BOOT/GRUB.CFG",
                "BOOT/EMPTY",
                "MD5SUM.TXT",
                "EMPTY.TXT"
            ]
        );
        assert_eq!(nodes[0].kind, Kind::Dir);
        assert_eq!(read(&image, &nodes[1]), b"menuentry");
        assert_eq!(nodes[4].size, 0);
        assert!(nodes[4].extents.is_empty());
    }

    #[test]
    fn the_joliet_tree_gives_each_name_as_it_was() {
        let items = [
            Item::Dir("Sources"),
            Item::File("Sources/\u{dc}berpr\u{fc}fung.txt", b"x"),
            Item::File("A long name with spaces.txt", b"yy"),
        ];
        let image = iso9660(&items, Iso { joliet: true });
        let nodes = tree(image.clone(), true).unwrap();
        assert_eq!(
            paths(&nodes),
            [
                "Sources",
                "Sources/\u{dc}berpr\u{fc}fung.txt",
                "A long name with spaces.txt"
            ]
        );
        assert_eq!(read(&image, &nodes[2]), b"yy");
    }

    #[test]
    fn a_directory_of_many_records_spans_its_sectors() {
        let names: Vec<String> = (0..200).map(|n| format!("dir/file{n:03}.txt")).collect();
        let mut items = vec![Item::Dir("dir")];
        items.extend(names.iter().map(|n| Item::File(n, b"z")));
        let nodes = tree(iso9660(&items, Iso::default()), false).unwrap();
        assert_eq!(nodes.len(), 201);
        assert_eq!(nodes[200].path.as_str(), "DIR/FILE199.TXT");
    }

    /// An image whose root holds these records after `.` and `..`.
    fn with_root(records: &[Vec<u8>], data_sectors: usize) -> Vec<u8> {
        let mut image = iso9660(&[], Iso::default());
        let d = read_descriptors(&mut Image::new(Cursor::new(image.clone()), "t").unwrap())
            .unwrap()
            .unwrap();
        let root = d.primary.root.extent as usize * 2048;
        let mut bytes = record_bytes(d.primary.root.extent, 2048, 0x02, &[0], &[]);
        bytes.extend(record_bytes(d.primary.root.extent, 2048, 0x02, &[1], &[]));
        for r in records {
            bytes.extend_from_slice(r);
        }
        image[root..root + bytes.len()].copy_from_slice(&bytes);
        image.resize(image.len() + data_sectors * 2048, 0x5A);
        image
    }

    #[test]
    fn a_file_of_several_extents_is_one_file() {
        let base = iso9660(&[], Iso::default()).len() as u32 / 2048;
        let image = with_root(
            &[
                record_bytes(base, 2048, MULTI_EXTENT, b"BIG.BIN;1", b""),
                record_bytes(base + 1, 2048, MULTI_EXTENT, b"BIG.BIN;1", b""),
                record_bytes(base + 2, 100, 0, b"BIG.BIN;1", b""),
            ],
            3,
        );
        let nodes = tree(image, false).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].size, 4196);
        assert_eq!(nodes[0].extents.len(), 3);
    }

    #[test]
    fn a_file_whose_last_extent_is_missing_is_refused() {
        let base = iso9660(&[], Iso::default()).len() as u32 / 2048;
        let image = with_root(
            &[record_bytes(base, 2048, MULTI_EXTENT, b"BIG.BIN;1", b"")],
            1,
        );
        let e = tree(image, false).unwrap_err();
        assert!(e.to_string().contains("its last extent is missing"), "{e}");
    }

    #[test]
    fn a_name_twice_in_one_directory_is_refused() {
        let base = iso9660(&[], Iso::default()).len() as u32 / 2048;
        let image = with_root(
            &[
                record_bytes(base, 10, 0, b"A.TXT;1", b""),
                record_bytes(base, 10, 0, b"A.TXT;2", b""),
            ],
            1,
        );
        let e = tree(image, false).unwrap_err();
        assert!(
            e.to_string().contains("holds the name \"A.TXT\" twice"),
            "{e}"
        );
    }

    #[test]
    fn a_directory_that_holds_the_root_is_refused() {
        let image = iso9660(&[], Iso::default());
        let d = read_descriptors(&mut Image::new(Cursor::new(image.clone()), "t").unwrap())
            .unwrap()
            .unwrap();
        let loop_back = record_bytes(d.primary.root.extent, 2048, 0x02, b"LOOP", b"");
        let e = tree(with_root(&[loop_back], 0), false).unwrap_err();
        assert!(e.to_string().contains("holds a directory above it"), "{e}");
    }

    #[test]
    fn a_file_past_the_end_of_the_image_is_refused() {
        let image = with_root(&[record_bytes(100_000, 10, 0, b"GONE.BIN;1", b"")], 0);
        let e = tree(image, false).unwrap_err();
        assert!(e.to_string().contains("GONE.BIN runs past the end"), "{e}");
    }

    #[test]
    fn an_associated_file_is_not_part_of_the_tree() {
        let image = with_root(&[record_bytes(0, 0, ASSOCIATED, b"FORK;1", b"")], 0);
        assert!(tree(image, false).unwrap().is_empty());
    }
}
