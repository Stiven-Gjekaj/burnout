//! Images that the tests build, so that each test holds the state it needs.
//!
//! These builders write only what the readers here need, and they write it
//! as the standards say. The gate in CI reads images that the tools of each
//! host make, so a fault that a builder and a reader share shows up there.

use std::collections::BTreeMap;

/// One entry of a tree that a test puts into an image.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Item<'a> {
    Dir(&'a str),
    File(&'a str, &'a [u8]),
}

impl Item<'_> {
    fn path(&self) -> &str {
        match self {
            Item::Dir(path) | Item::File(path, _) => path,
        }
    }
}

/// Which trees an ISO 9660 image carries beside the primary one.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Iso {
    pub joliet: bool,
}

const SECTOR: usize = 2048;

/// The bytes of one directory record of ISO 9660.
pub(crate) fn record_bytes(
    extent: u32,
    length: u32,
    flags: u8,
    id: &[u8],
    system_use: &[u8],
) -> Vec<u8> {
    let pad = 1 - id.len() % 2;
    let size = 33 + id.len() + pad + system_use.len();
    assert!(size <= 255, "a record of {size} bytes");
    let mut r = vec![0u8; size];
    r[0] = size as u8;
    r[2..6].copy_from_slice(&extent.to_le_bytes());
    r[6..10].copy_from_slice(&extent.to_be_bytes());
    r[10..14].copy_from_slice(&length.to_le_bytes());
    r[14..18].copy_from_slice(&length.to_be_bytes());
    r[25] = flags;
    r[28..30].copy_from_slice(&1u16.to_le_bytes());
    r[30..32].copy_from_slice(&1u16.to_be_bytes());
    r[32] = id.len() as u8;
    r[33..33 + id.len()].copy_from_slice(id);
    r[33 + id.len() + pad..].copy_from_slice(system_use);
    r
}

/// Records packed into whole sectors, as a directory holds them: a record
/// that does not fit the rest of a sector starts the next one.
fn pack(records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for r in records {
        let used = bytes.len() % SECTOR;
        if used + r.len() > SECTOR {
            bytes.resize(bytes.len() + SECTOR - used, 0);
        }
        bytes.extend_from_slice(r);
    }
    bytes.resize(bytes.len().div_ceil(SECTOR).max(1) * SECTOR, 0);
    bytes
}

/// A tree of one kind of names: the primary one or the Joliet one.
struct Names {
    joliet: bool,
}

impl Names {
    fn id(&self, name: &str, is_file: bool) -> Vec<u8> {
        let name = if is_file {
            format!("{name};1")
        } else {
            name.to_string()
        };
        if self.joliet {
            name.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
        } else {
            name.to_uppercase().into_bytes()
        }
    }
}

/// An ISO 9660 image that holds these items. Each directory comes before
/// what it holds, as a tree gives them.
pub(crate) fn iso9660(items: &[Item], options: Iso) -> Vec<u8> {
    // The directories, the root first, and what each holds.
    let mut dirs: Vec<&str> = vec![""];
    let mut children: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (at, item) in items.iter().enumerate() {
        let parent = item.path().rsplit_once('/').map_or("", |(p, _)| p);
        children.entry(parent).or_default().push(at);
        if let Item::Dir(path) = item {
            dirs.push(path);
        }
    }
    let trees: Vec<Names> = std::iter::once(Names { joliet: false })
        .chain(options.joliet.then_some(Names { joliet: true }))
        .collect();

    // Sector 16 on: one descriptor for each tree, and the terminator.
    let mut next = 16 + trees.len() as u32 + 1;
    let record_set = |names: &Names,
                      dir: &str,
                      extents: &BTreeMap<(usize, &str), (u32, u32)>,
                      t: usize,
                      files: &[(u32, u32)]|
     -> Vec<Vec<u8>> {
        let parent = dir.rsplit_once('/').map_or("", |(p, _)| p);
        let (own, own_len) = extents.get(&(t, dir)).copied().unwrap_or((0, 0));
        let (up, up_len) = extents.get(&(t, parent)).copied().unwrap_or((0, 0));
        let mut records = vec![
            record_bytes(own, own_len, 0x02, &[0], &[]),
            record_bytes(up, up_len, 0x02, &[1], &[]),
        ];
        for at in children.get(dir).map(Vec::as_slice).unwrap_or(&[]) {
            let item = &items[*at];
            let name = item.path().rsplit('/').next().unwrap();
            let r = match item {
                Item::Dir(path) => {
                    let (e, l) = extents.get(&(t, *path)).copied().unwrap_or((0, 0));
                    record_bytes(e, l, 0x02, &names.id(name, false), &[])
                }
                Item::File(_, bytes) => {
                    let (e, _) = files[*at];
                    record_bytes(e, bytes.len() as u32, 0, &names.id(name, true), &[])
                }
            };
            records.push(r);
        }
        records
    };

    // The sizes of the directories do not depend on where they go, so one
    // pass finds them and the next writes them.
    let no_extents = BTreeMap::new();
    let no_files = vec![(0u32, 0u32); items.len()];
    let mut extents: BTreeMap<(usize, &str), (u32, u32)> = BTreeMap::new();
    for (t, names) in trees.iter().enumerate() {
        for dir in &dirs {
            let size = pack(&record_set(names, dir, &no_extents, t, &no_files)).len();
            extents.insert((t, dir), (next, size as u32));
            next += (size / SECTOR) as u32;
        }
    }
    let mut files = vec![(0u32, 0u32); items.len()];
    for (at, item) in items.iter().enumerate() {
        if let Item::File(_, bytes) = item {
            if !bytes.is_empty() {
                files[at] = (next, bytes.len() as u32);
                next += bytes.len().div_ceil(SECTOR) as u32;
            }
        }
    }

    let mut image = vec![0u8; next as usize * SECTOR];
    for (t, names) in trees.iter().enumerate() {
        let (root, root_len) = extents[&(t, "")];
        let d = &mut image[(16 + t) * SECTOR..(17 + t) * SECTOR];
        d[0] = if names.joliet { 2 } else { 1 };
        d[1..6].copy_from_slice(b"CD001");
        d[6] = 1;
        d[40..72].fill(b' ');
        d[40..44].copy_from_slice(b"TEST");
        d[80..84].copy_from_slice(&next.to_le_bytes());
        d[84..88].copy_from_slice(&next.to_be_bytes());
        if names.joliet {
            d[88..91].copy_from_slice(b"%/E");
        }
        d[128..130].copy_from_slice(&2048u16.to_le_bytes());
        d[130..132].copy_from_slice(&2048u16.to_be_bytes());
        d[156..190].copy_from_slice(&record_bytes(root, root_len, 0x02, &[0], &[]));
        for dir in &dirs {
            let (extent, _) = extents[&(t, *dir)];
            let bytes = pack(&record_set(names, dir, &extents, t, &files));
            let at = extent as usize * SECTOR;
            image[at..at + bytes.len()].copy_from_slice(&bytes);
        }
    }
    let end = 16 + trees.len();
    image[end * SECTOR] = 255;
    image[end * SECTOR + 1..end * SECTOR + 6].copy_from_slice(b"CD001");
    image[end * SECTOR + 6] = 1;
    for (at, item) in items.iter().enumerate() {
        if let Item::File(_, bytes) = item {
            let start = files[at].0 as usize * SECTOR;
            image[start..start + bytes.len()].copy_from_slice(bytes);
        }
    }
    image
}
