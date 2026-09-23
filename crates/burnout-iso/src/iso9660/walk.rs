//! A walk of one tree of ISO 9660, from its root down.

use std::collections::HashSet;
use std::io::{Read, Seek};

use burnout_core::{Result, TreePath};

use super::descriptors::Root;
use super::directory::{joliet_name, plain_name, records, Record, ASSOCIATED, MULTI_EXTENT};
use super::rock_ridge::read_entries;
use crate::image::{Image, SECTOR};
use crate::node::{Extent, Kind, Node, DEEPEST};

/// Which names a tree keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Names {
    Plain,
    Joliet,
    /// The names, links and modes of Rock Ridge, over the primary tree, with
    /// the bytes to skip in each system use area.
    RockRidge {
        skip: u8,
    },
}

/// The kinds of file in a POSIX mode.
const MODE_TYPE: u32 = 0o170000;
const MODE_DIR: u32 = 0o040000;
const MODE_FILE: u32 = 0o100000;
const MODE_LINK: u32 = 0o120000;

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
    // A file of more than one extent is a run of records with one
    // identifier, and each record but the last carries the flag.
    let mut pending: Option<(Vec<u8>, Node)> = None;
    for r in records {
        if r.is_self() || r.is_parent() || r.flags & ASSOCIATED != 0 {
            continue;
        }
        if let Some((id, node)) = pending.as_mut() {
            if *id != r.id {
                return Err(image.fault(format!("{}: its last extent is missing", node.path)));
            }
            push_extent(image, node, &r)?;
            if r.flags & MULTI_EXTENT == 0 {
                out.extend(pending.take().map(|(_, node)| node));
            }
            continue;
        }
        let rr = match names {
            Names::RockRidge { skip } => {
                let what = format!(
                    "the record {:?} of {at_dir}",
                    String::from_utf8_lossy(&r.id)
                );
                Some(read_entries(image, &r.system_use, skip as usize, &what)?)
            }
            _ => None,
        };
        // A directory that Rock Ridge moved belongs where its child link is.
        // A device, a pipe or a socket is nothing that a drive can hold.
        if let Some(e) = &rr {
            let kind = e.mode.map_or(0, |m| m & MODE_TYPE);
            if e.relocated || ![0, MODE_DIR, MODE_FILE, MODE_LINK].contains(&kind) {
                continue;
            }
        }
        let name = match (names, rr.as_ref().and_then(|e| e.name())) {
            (_, Some(name)) => name,
            (Names::Joliet, None) => joliet_name(&r.id),
            (_, None) => plain_name(&r.id),
        }
        .map_err(|e| image.fault(format!("{at_dir}: {e}")))?;
        let path = match above {
            Some(above) => above.join(&name)?,
            None => TreePath::new(&name)?,
        };
        if !taken.insert(name.clone()) {
            return Err(image.fault(format!("{at_dir} holds the name {name:?} twice")));
        }
        if let Some(target) = rr.as_ref().and_then(|e| e.link()) {
            out.push(Node {
                path,
                kind: Kind::Link(target),
                size: 0,
                extents: Vec::new(),
            });
            continue;
        }
        if let Some(block) = rr.as_ref().and_then(|e| e.child_link) {
            let inner = moved_dir(image, block, &path)?;
            out.push(Node {
                path: path.clone(),
                kind: Kind::Dir,
                size: 0,
                extents: Vec::new(),
            });
            walk_dir(image, inner, Some(&path), names, seen, out, depth + 1)?;
            continue;
        }
        if let Some(e) = &rr {
            if !r.is_dir() && (e.compressed || e.sparse) {
                let how = if e.compressed {
                    "compressed with zisofs"
                } else {
                    "sparse"
                };
                return Err(image.fault(format!("{path} is {how}, and Burnout reads no such file")));
            }
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
            pending = Some((r.id, node));
        } else {
            out.push(node);
        }
    }
    if let Some((_, node)) = pending {
        return Err(image.fault(format!("{}: its last extent is missing", node.path)));
    }
    Ok(())
}

/// The directory that a child link names: its first record gives where it
/// is and how long it is.
fn moved_dir<R: Read + Seek>(image: &mut Image<R>, block: u32, path: &TreePath) -> Result<Root> {
    let first = image.read_at(block as u64 * SECTOR, SECTOR as usize, &format!("{path}"))?;
    let dot = records(&first)
        .map_err(|e| image.fault(format!("{path}: {e}")))?
        .into_iter()
        .find(Record::is_self)
        .ok_or_else(|| {
            image.fault(format!(
                "{path}: the directory it moved to has no first record"
            ))
        })?;
    Ok(Root {
        extent: dot.extent,
        length: dot.length,
    })
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
    use super::super::directory::DIRECTORY;
    use super::*;
    use crate::iso9660::{detect_rock_ridge, read_descriptors};
    use crate::testing::{both, iso9660, nm, put, px, record_bytes, rr_root, susp, Iso, Item};
    use std::io::Cursor;

    fn tree(image: Vec<u8>, joliet: bool) -> Result<Vec<Node>> {
        let mut i = Image::new(Cursor::new(image), "test.iso").unwrap();
        let d = read_descriptors(&mut i).unwrap().unwrap();
        match joliet {
            true => walk(&mut i, d.joliet.unwrap().root, Names::Joliet),
            false => walk(&mut i, d.primary.root, Names::Plain),
        }
    }

    fn rock_ridge_tree(image: Vec<u8>) -> Result<Vec<Node>> {
        let mut i = Image::new(Cursor::new(image), "test.iso").unwrap();
        let d = read_descriptors(&mut i).unwrap().unwrap();
        let skip = detect_rock_ridge(&mut i, d.primary.root)?.expect("Rock Ridge");
        walk(&mut i, d.primary.root, Names::RockRidge { skip })
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
        let image = iso9660(
            &items,
            Iso {
                joliet: true,
                ..Iso::default()
            },
        );
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

    /// Where the root of an empty image is, and the first block past its end.
    fn blank() -> (u32, u32) {
        let image = iso9660(&[], Iso::default());
        let d = read_descriptors(&mut Image::new(Cursor::new(image.clone()), "t").unwrap())
            .unwrap()
            .unwrap();
        (d.primary.root.extent, image.len() as u32 / 2048)
    }

    /// An empty image whose root holds these records after `.` and `..`, with
    /// `dot` as the system use area of `.`, and blocks of data after it.
    fn with_root(dot: &[u8], records: &[Vec<u8>], data_sectors: usize) -> Vec<u8> {
        let mut image = iso9660(&[], Iso::default());
        let (root, _) = blank();
        let mut bytes = record_bytes(root, 2048, DIRECTORY, &[0], dot);
        bytes.extend(record_bytes(root, 2048, DIRECTORY, &[1], &[]));
        for r in records {
            bytes.extend_from_slice(r);
        }
        put(&mut image, root, &bytes);
        image.resize(image.len() + data_sectors * 2048, 0x5A);
        image
    }

    #[test]
    fn a_file_of_several_extents_is_one_file() {
        let (_, base) = blank();
        let image = with_root(
            &[],
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
        let (_, base) = blank();
        let image = with_root(
            &[],
            &[record_bytes(base, 2048, MULTI_EXTENT, b"BIG.BIN;1", b"")],
            1,
        );
        let e = tree(image, false).unwrap_err();
        assert!(e.to_string().contains("its last extent is missing"), "{e}");
    }

    #[test]
    fn a_name_twice_in_one_directory_is_refused() {
        let (_, base) = blank();
        let image = with_root(
            &[],
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
        let (root, _) = blank();
        let loop_back = record_bytes(root, 2048, DIRECTORY, b"LOOP", b"");
        let e = tree(with_root(&[], &[loop_back], 0), false).unwrap_err();
        assert!(e.to_string().contains("holds a directory above it"), "{e}");
    }

    #[test]
    fn a_file_past_the_end_of_the_image_is_refused() {
        let image = with_root(&[], &[record_bytes(100_000, 10, 0, b"GONE.BIN;1", b"")], 0);
        let e = tree(image, false).unwrap_err();
        assert!(e.to_string().contains("GONE.BIN runs past the end"), "{e}");
    }

    #[test]
    fn an_associated_file_is_not_part_of_the_tree() {
        let image = with_root(&[], &[record_bytes(0, 0, ASSOCIATED, b"FORK;1", b"")], 0);
        assert!(tree(image, false).unwrap().is_empty());
    }

    const LINKS: &[Item] = &[
        Item::Dir("boot"),
        Item::File("boot/grub.cfg", b"menuentry"),
        Item::Link("boot/latest", "../A Name with Case.txt"),
        Item::File("A Name with Case.txt", b"yy"),
        Item::Link("etc", "/usr/etc"),
    ];

    #[test]
    fn rock_ridge_gives_the_names_and_the_links() {
        let options = Iso {
            joliet: true,
            rock_ridge: true,
        };
        let image = iso9660(LINKS, options);
        let nodes = rock_ridge_tree(image.clone()).unwrap();
        assert_eq!(
            paths(&nodes),
            [
                "boot",
                "boot/grub.cfg",
                "boot/latest",
                "A Name with Case.txt",
                "etc"
            ]
        );
        assert_eq!(read(&image, &nodes[1]), b"menuentry");
        assert_eq!(nodes[2].kind, Kind::Link("../A Name with Case.txt".into()));
        assert_eq!(nodes[4].kind, Kind::Link("/usr/etc".into()));
        assert_eq!(read(&image, &nodes[3]), b"yy");
    }

    #[test]
    fn the_joliet_tree_of_the_same_image_holds_no_link() {
        let options = Iso {
            joliet: true,
            rock_ridge: true,
        };
        let nodes = tree(iso9660(LINKS, options), true).unwrap();
        assert_eq!(
            paths(&nodes),
            ["boot", "boot/grub.cfg", "A Name with Case.txt"]
        );
    }

    #[test]
    fn a_name_in_a_continuation_area_is_read() {
        let (_, base) = blank();
        let area = nm("a name that the record had no room for.txt");
        let ce = [both(base), both(100), both(area.len() as u32)].concat();
        let su = [px(0o100644), susp(b"CE", &ce)].concat();
        let record = record_bytes(0, 0, 0, b"I0000;1", &su);
        let mut image = with_root(&rr_root(0), &[record], 1);
        let at = base as usize * 2048 + 100;
        image[at..at + area.len()].copy_from_slice(&area);
        let nodes = rock_ridge_tree(image).unwrap();
        assert_eq!(
            paths(&nodes),
            ["a name that the record had no room for.txt"]
        );
    }

    #[test]
    fn a_moved_directory_is_where_its_child_link_is() {
        let (root, base) = blank();
        let (moved, deep, data) = (base, base + 1, base + 2);
        let dir_su = |name: &str| [px(0o040755), nm(name)].concat();
        let stand_in = [dir_su("deep"), susp(b"CL", &both(deep))].concat();
        let records = [
            record_bytes(0, 0, 0, b"I0000;1", &stand_in),
            record_bytes(moved, 2048, DIRECTORY, b"I0001", &dir_su("rr_moved")),
        ];
        let mut image = with_root(&rr_root(0), &records, 3);
        let mut rr_moved = record_bytes(moved, 2048, DIRECTORY, &[0], &[]);
        rr_moved.extend(record_bytes(root, 2048, DIRECTORY, &[1], &[]));
        let relocated = [dir_su("deep"), susp(b"RE", &[])].concat();
        rr_moved.extend(record_bytes(deep, 2048, DIRECTORY, b"I0002", &relocated));
        put(&mut image, moved, &rr_moved);
        let mut inner = record_bytes(deep, 2048, DIRECTORY, &[0], &[]);
        let parent_link = susp(b"PL", &both(root));
        inner.extend(record_bytes(moved, 2048, DIRECTORY, &[1], &parent_link));
        let file_su = [px(0o100644), nm("inner.txt")].concat();
        inner.extend(record_bytes(data, 5, 0, b"I0003;1", &file_su));
        put(&mut image, deep, &inner);
        put(&mut image, data, b"inner");

        let nodes = rock_ridge_tree(image.clone()).unwrap();
        assert_eq!(paths(&nodes), ["deep", "deep/inner.txt", "rr_moved"]);
        assert_eq!(nodes[0].kind, Kind::Dir);
        assert_eq!(read(&image, &nodes[1]), b"inner");
    }

    #[test]
    fn a_file_compressed_with_zisofs_is_refused() {
        let (_, base) = blank();
        let zf = susp(b"ZF", &[&b"pz"[..], &[4, 15], &both(4096)].concat());
        let su = [px(0o100644), nm("vmlinuz"), zf].concat();
        let image = with_root(
            &rr_root(0),
            &[record_bytes(base, 10, 0, b"I0000;1", &su)],
            1,
        );
        let e = rock_ridge_tree(image).unwrap_err();
        assert!(
            e.to_string().contains("vmlinuz is compressed with zisofs"),
            "{e}"
        );
    }

    #[test]
    fn the_bytes_to_skip_come_before_the_entries_of_each_record() {
        let su = [vec![0xAA, 0xBB, 0xCC], px(0o100644), nm("after.txt")].concat();
        let image = with_root(&rr_root(3), &[record_bytes(0, 0, 0, b"I0000;1", &su)], 0);
        assert_eq!(paths(&rock_ridge_tree(image).unwrap()), ["after.txt"]);
    }

    #[test]
    fn a_device_is_not_part_of_the_tree() {
        let su = [px(0o020620), nm("console")].concat();
        let image = with_root(&rr_root(0), &[record_bytes(0, 0, 0, b"I0000;1", &su)], 0);
        assert!(rock_ridge_tree(image).unwrap().is_empty());
    }

    #[test]
    fn a_rock_ridge_name_with_a_slash_is_refused() {
        let su = [px(0o100644), nm("etc/passwd")].concat();
        let image = with_root(&rr_root(0), &[record_bytes(0, 0, 0, b"I0000;1", &su)], 0);
        let e = rock_ridge_tree(image).unwrap_err();
        assert!(e.to_string().contains("\"etc/passwd\""), "{e}");
    }

    #[test]
    fn a_file_of_several_extents_takes_the_name_of_its_first_record() {
        let (_, base) = blank();
        let first = [px(0o100644), nm("big.bin")].concat();
        let records = [
            record_bytes(base, 2048, MULTI_EXTENT, b"I0000;1", &first),
            record_bytes(base + 1, 100, 0, b"I0000;1", &px(0o100644)),
        ];
        let nodes = rock_ridge_tree(with_root(&rr_root(0), &records, 2)).unwrap();
        assert_eq!(paths(&nodes), ["big.bin"]);
        assert_eq!(nodes[0].size, 2148);
    }
}
