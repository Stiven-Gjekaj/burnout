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
    /// A link and the path that it names. Only Rock Ridge holds a link, so
    /// the other trees leave it out.
    Link(&'a str, &'a str),
}

impl Item<'_> {
    fn path(&self) -> &str {
        match self {
            Item::Dir(path) | Item::File(path, _) | Item::Link(path, _) => path,
        }
    }
}

/// Which trees an ISO 9660 image carries beside the primary one.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Iso {
    pub joliet: bool,
    /// Rock Ridge entries on the primary tree. Its plain names are then only
    /// numbers, so a name that a walk gives can only come from Rock Ridge.
    pub rock_ridge: bool,
}

const SECTOR: usize = 2048;

/// A value in both byte orders, the low byte first.
pub(crate) fn both(value: u32) -> Vec<u8> {
    [value.to_le_bytes(), value.to_be_bytes()].concat()
}

/// One entry of a system use area.
pub(crate) fn susp(signature: &[u8; 2], data: &[u8]) -> Vec<u8> {
    let mut e = vec![signature[0], signature[1], (4 + data.len()) as u8, 1];
    e.extend_from_slice(data);
    e
}

/// The entries of the first record of a root with Rock Ridge: the protocol,
/// with the bytes to skip in each other record, and the extension.
pub(crate) fn rr_root(skip: u8) -> Vec<u8> {
    let mut su = susp(b"SP", &[0xBE, 0xEF, skip]);
    let (id, about, source) = (b"RRIP_1991A", b"ROCK RIDGE", b"TEST");
    let mut er = vec![id.len() as u8, about.len() as u8, source.len() as u8, 1];
    er.extend_from_slice(id);
    er.extend_from_slice(about);
    er.extend_from_slice(source);
    su.extend(susp(b"ER", &er));
    su
}

/// The POSIX mode of an entry, with one link and no owner.
pub(crate) fn px(mode: u32) -> Vec<u8> {
    let data: Vec<u8> = [mode, 1, 0, 0].into_iter().flat_map(both).collect();
    susp(b"PX", &data)
}

/// The name of an entry.
pub(crate) fn nm(name: &str) -> Vec<u8> {
    susp(b"NM", &[&[0], name.as_bytes()].concat())
}

/// A link to `target`: one component for each name, and flags for the root,
/// for `.` and for `..`.
pub(crate) fn sl(target: &str) -> Vec<u8> {
    let mut data = vec![0];
    let rest = match target.strip_prefix('/') {
        Some(rest) => {
            data.extend([0x08, 0]);
            rest
        }
        None => target,
    };
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        match part {
            "." => data.extend([0x02, 0]),
            ".." => data.extend([0x04, 0]),
            _ => {
                data.extend([0, part.len() as u8]);
                data.extend_from_slice(part.as_bytes());
            }
        }
    }
    susp(b"SL", &data)
}

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
    rock_ridge: bool,
}

impl Names {
    /// The identifier of the item at `at` of the items.
    fn id(&self, at: usize, name: &str, is_file: bool) -> Vec<u8> {
        let name = match self.rock_ridge {
            true => format!("I{at:04}"),
            false => name.to_string(),
        };
        let name = if is_file { format!("{name};1") } else { name };
        if self.joliet {
            name.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
        } else {
            name.to_uppercase().into_bytes()
        }
    }

    /// The system use area of the record of an item.
    fn system_use(&self, item: &Item, name: &str) -> Vec<u8> {
        if !self.rock_ridge {
            return Vec::new();
        }
        let (mode, target) = match item {
            Item::Dir(_) => (0o040755, None),
            Item::File(..) => (0o100644, None),
            Item::Link(_, target) => (0o120777, Some(*target)),
        };
        let mut su = px(mode);
        su.extend(nm(name));
        su.extend(target.map(sl).unwrap_or_default());
        su
    }
}

/// Write bytes into one block of an image, and zeros into the rest of it.
pub(crate) fn put(image: &mut [u8], block: u32, bytes: &[u8]) {
    let at = block as usize * SECTOR;
    image[at..at + SECTOR].fill(0);
    image[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A descriptor of UDF: a tag of version 2 for `id` at block `location`,
/// with the checksum and the CRC of `body`, and then the body.
pub(crate) fn tagged(id: u16, location: u32, body: &[u8]) -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..2].copy_from_slice(&id.to_le_bytes());
    d[2..4].copy_from_slice(&2u16.to_le_bytes());
    d[10..12].copy_from_slice(&(body.len() as u16).to_le_bytes());
    d[12..16].copy_from_slice(&location.to_le_bytes());
    d.extend_from_slice(body);
    retag(&mut d);
    d
}

/// Set the CRC and the checksum of a descriptor again, after a test
/// changes a byte of it.
pub(crate) fn retag(d: &mut [u8]) {
    let length = u16::from_le_bytes([d[10], d[11]]) as usize;
    let crc = crate::udf::crc_itu_t(&d[16..16 + length]);
    d[8..10].copy_from_slice(&crc.to_le_bytes());
    d[4] = 0;
    d[4] = d[..16].iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
}

/// Where the builder of UDF puts the main sequence, its reserve copy and the
/// partition.
pub(crate) const UDF_MAIN: u32 = 257;
pub(crate) const UDF_RESERVE: u32 = 273;
pub(crate) const UDF_PARTITION: u32 = 289;

/// How the builder of UDF records where the data of each entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ads {
    Short,
    Long,
    /// The data in the entry, when it fits there, and short descriptors
    /// when it does not.
    Embedded,
}

/// Choices for an image of UDF.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Udf {
    pub label: &'static str,
    /// The descriptor that says that UDF is there: NSR02 or NSR03.
    pub nsr: &'static [u8; 5],
    /// Extended file entries, as UDF 2.00 and later write them.
    pub extended: bool,
    pub ads: Ads,
}

impl Default for Udf {
    fn default() -> Self {
        Udf {
            label: "TEST",
            nsr: b"NSR02",
            extended: false,
            ads: Ads::Short,
        }
    }
}

/// Text in CS0: 8 bits for each character when all of them fit, else 16.
pub(crate) fn cs0(text: &str) -> Vec<u8> {
    if text.chars().all(|c| (c as u32) < 256) {
        std::iter::once(8)
            .chain(text.chars().map(|c| c as u8))
            .collect()
    } else {
        std::iter::once(16)
            .chain(text.encode_utf16().flat_map(|u| u.to_be_bytes()))
            .collect()
    }
}

/// CS0 text in a field of `length` bytes, with its length in the last byte.
pub(crate) fn dstring(text: &str, length: usize) -> Vec<u8> {
    let mut field = cs0(text);
    let used = field.len();
    assert!(used < length, "{text:?} does not fit {length} bytes");
    field.resize(length - 1, 0);
    field.push(used as u8);
    field
}

fn write_at(bytes: &mut [u8], at: usize, value: &[u8]) {
    bytes[at..at + value.len()].copy_from_slice(value);
}

/// A logical volume descriptor with its partition maps, and its file set at
/// a block of the first partition.
pub(crate) fn logical_volume(
    location: u32,
    number: u32,
    label: &str,
    maps: &[Vec<u8>],
    file_set: u32,
) -> Vec<u8> {
    let table: Vec<u8> = maps.concat();
    // The body starts after the tag, so each place is 16 less than in the
    // descriptor.
    let mut b = vec![0u8; 440 - 16];
    write_at(&mut b, 0, &number.to_le_bytes());
    write_at(&mut b, 5, b"OSTA Compressed Unicode");
    write_at(&mut b, 68, &dstring(label, 128));
    write_at(&mut b, 196, &2048u32.to_le_bytes());
    write_at(&mut b, 201, b"*OSTA UDF Compliant");
    write_at(&mut b, 224, &0x0102u16.to_le_bytes());
    write_at(&mut b, 232, &2048u32.to_le_bytes());
    write_at(&mut b, 236, &file_set.to_le_bytes());
    write_at(&mut b, 248, &(table.len() as u32).to_le_bytes());
    write_at(&mut b, 252, &(maps.len() as u32).to_le_bytes());
    b.extend(table);
    tagged(6, location, &b)
}

/// A partition map of type 1, which names a partition by its number.
pub(crate) fn type1_map(number: u16) -> Vec<u8> {
    let [low, high] = number.to_le_bytes();
    vec![1, 6, 1, 0, low, high]
}

/// A partition map of type 2, with the identifier of what it is.
pub(crate) fn type2_map(identifier: &str) -> Vec<u8> {
    let mut map = vec![0u8; 64];
    map[0] = 2;
    map[1] = 64;
    write_at(&mut map, 5, identifier.as_bytes());
    map
}

/// A partition descriptor of UDF.
pub(crate) fn partition(location: u32, number: u32, id: u16, start: u32, length: u32) -> Vec<u8> {
    let mut b = vec![0u8; 512 - 16];
    write_at(&mut b, 0, &number.to_le_bytes());
    write_at(&mut b, 4, &1u16.to_le_bytes());
    write_at(&mut b, 6, &id.to_le_bytes());
    write_at(&mut b, 9, b"+NSR02");
    write_at(&mut b, 168, &1u32.to_le_bytes());
    write_at(&mut b, 172, &start.to_le_bytes());
    write_at(&mut b, 176, &length.to_le_bytes());
    tagged(5, location, &b)
}

/// An anchor that names a main sequence and a reserve copy of 16 sectors.
pub(crate) fn anchor(location: u32, main: u32, reserve: u32) -> Vec<u8> {
    let mut b = vec![0u8; 496];
    write_at(&mut b, 0, &(16 * 2048u32).to_le_bytes());
    write_at(&mut b, 4, &main.to_le_bytes());
    write_at(&mut b, 8, &(16 * 2048u32).to_le_bytes());
    write_at(&mut b, 12, &reserve.to_le_bytes());
    tagged(2, location, &b)
}

/// A volume descriptor pointer to the next part of a sequence.
pub(crate) fn pointer(location: u32, number: u32, next: u32, sectors: u32) -> Vec<u8> {
    let mut b = vec![0u8; 496];
    write_at(&mut b, 0, &number.to_le_bytes());
    write_at(&mut b, 4, &(sectors * 2048).to_le_bytes());
    write_at(&mut b, 8, &next.to_le_bytes());
    tagged(3, location, &b)
}

/// The descriptor that ends a sequence.
pub(crate) fn terminator(location: u32) -> Vec<u8> {
    tagged(8, location, &[0; 496])
}

/// A file entry of UDF, or an extended one, of the given type and size, with
/// 24 bytes of extended attributes before the allocation descriptors. The
/// type of the descriptors is 0 for short, 1 for long and 3 for data in the
/// entry.
pub(crate) fn file_entry(
    location: u32,
    extended: bool,
    file_type: u8,
    size: u64,
    descriptors: u16,
    area: &[u8],
) -> Vec<u8> {
    let (id, base, lengths) = match extended {
        true => (266, 216, 208),
        false => (261, 176, 168),
    };
    let mut b = vec![0u8; base - 16];
    write_at(&mut b, 4, &4u16.to_le_bytes());
    b[11] = file_type;
    write_at(&mut b, 18, &descriptors.to_le_bytes());
    write_at(&mut b, 40, &size.to_le_bytes());
    write_at(&mut b, lengths - 16, &24u32.to_le_bytes());
    write_at(&mut b, lengths - 12, &(area.len() as u32).to_le_bytes());
    b.extend([0xEA; 24]);
    b.extend_from_slice(area);
    tagged(id, location, &b)
}

/// A short allocation descriptor: the type of the run in the two high bits
/// of its length, then its first block.
pub(crate) fn short_ad(kind: u32, length: u32, block: u32) -> Vec<u8> {
    [(kind << 30 | length).to_le_bytes(), block.to_le_bytes()].concat()
}

/// A long allocation descriptor, which names its partition.
pub(crate) fn long_ad(kind: u32, length: u32, block: u32, partition: u16) -> Vec<u8> {
    let mut ad = short_ad(kind, length, block);
    ad.extend(partition.to_le_bytes());
    ad.extend([0; 6]);
    ad
}

/// An allocation extent descriptor, which holds more descriptors.
pub(crate) fn allocation_extent(location: u32, area: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 8];
    write_at(&mut b, 4, &(area.len() as u32).to_le_bytes());
    b.extend_from_slice(area);
    tagged(258, location, &b)
}

/// A file set descriptor whose root has its file entry at block `root`.
pub(crate) fn file_set(location: u32, root: u32) -> Vec<u8> {
    let mut b = vec![0u8; 512 - 16];
    write_at(&mut b, 400 - 16, &2048u32.to_le_bytes());
    write_at(&mut b, 404 - 16, &root.to_le_bytes());
    write_at(&mut b, 417 - 16, b"*OSTA UDF Compliant");
    tagged(256, location, &b)
}

/// A file identifier descriptor that names the file entry at block `icb`.
/// The name of the parent is empty.
pub(crate) fn identifier(location: u32, characteristics: u8, name: &str, icb: u32) -> Vec<u8> {
    let name = match name.is_empty() {
        true => Vec::new(),
        false => cs0(name),
    };
    let length = (38 + name.len()).div_ceil(4) * 4;
    let mut b = vec![0u8; length - 16];
    write_at(&mut b, 0, &1u16.to_le_bytes());
    b[2] = characteristics;
    b[3] = name.len() as u8;
    write_at(&mut b, 4, &2048u32.to_le_bytes());
    write_at(&mut b, 8, &icb.to_le_bytes());
    write_at(&mut b, 22, &name);
    tagged(257, location, &b)
}

/// The data of a link to `target`: a component for the root, for `..`, for
/// `.`, and for each name.
fn link_data(target: &str) -> Vec<u8> {
    let mut data = Vec::new();
    let rest = match target.strip_prefix('/') {
        Some(rest) => {
            data.extend([2, 0, 0, 0]);
            rest
        }
        None => target,
    };
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        match part {
            ".." => data.extend([3, 0, 0, 0]),
            "." => data.extend([4, 0, 0, 0]),
            name => {
                let name = cs0(name);
                data.extend([5, name.len() as u8, 0, 0]);
                data.extend(name);
            }
        }
    }
    data
}

/// The directory that holds a path of a tree, which is `""` for the root.
fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

/// An image of UDF that holds these items. Each directory comes before what
/// it holds.
///
/// Block 0 of the partition holds the file set descriptor, block 1 the file
/// entry of the root, and block `n + 2` the file entry of item `n`. The data
/// comes after the entries.
pub(crate) fn udf(items: &[Item], options: &Udf) -> Vec<u8> {
    let paths: Vec<&str> = std::iter::once("")
        .chain(items.iter().map(Item::path))
        .collect();
    let entry_of: BTreeMap<&str, usize> = paths.iter().enumerate().map(|(n, p)| (*p, n)).collect();
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (n, path) in paths.iter().enumerate().skip(1) {
        children
            .entry(entry_of[parent_of(path)])
            .or_default()
            .push(n);
    }
    let block = |n: usize| 1 + n as u32;
    let item = |n: usize| (n > 0).then(|| items[n - 1]);
    let is_dir = |n: usize| matches!(item(n), None | Some(Item::Dir(_)));

    // The identifiers of a directory, the parent first. Each one records the
    // block that it starts in: the entry itself when the data is there.
    let stream = |n: usize, first: Option<u32>| -> Vec<u8> {
        let location = |at: usize| first.map_or(block(n), |b| b + (at / SECTOR) as u32);
        let up = block(entry_of[parent_of(paths[n])]);
        let mut s = identifier(location(0), 0x0A, "", up);
        for &c in children.get(&n).map(Vec::as_slice).unwrap_or(&[]) {
            let name = paths[c].rsplit('/').next().unwrap();
            let characteristics = if is_dir(c) { 0x02 } else { 0 };
            let at = location(s.len());
            s.extend(identifier(at, characteristics, name, block(c)));
        }
        s
    };
    let data = |n: usize, first: Option<u32>| -> Vec<u8> {
        match item(n) {
            None | Some(Item::Dir(_)) => stream(n, first),
            Some(Item::File(_, bytes)) => bytes.to_vec(),
            Some(Item::Link(_, target)) => link_data(target),
        }
    };

    // The data of each entry goes into the entry, or into blocks after the
    // entries.
    let room = SECTOR - if options.extended { 216 } else { 176 } - 24;
    let mut next = block(paths.len());
    let mut place = Vec::new();
    for n in 0..paths.len() {
        let length = data(n, None).len();
        if length == 0 || (options.ads == Ads::Embedded && length <= room) {
            place.push(None);
        } else {
            place.push(Some(next));
            next += length.div_ceil(SECTOR) as u32;
        }
    }

    let mut image = udf_volume(options, next);
    put(&mut image, UDF_PARTITION, &file_set(0, block(0)));
    for (n, place) in place.into_iter().enumerate() {
        let bytes = data(n, place);
        let file_type = match item(n) {
            None | Some(Item::Dir(_)) => 4,
            Some(Item::File(..)) => 5,
            Some(Item::Link(..)) => 12,
        };
        let length = bytes.len() as u32;
        let (form, area) = match (place, options.ads) {
            (None, Ads::Embedded) => (3, bytes.clone()),
            (None, Ads::Long) => (1, Vec::new()),
            (None, _) => (0, Vec::new()),
            (Some(first), Ads::Long) => (1, long_ad(0, length, first, 0)),
            (Some(first), _) => (0, short_ad(0, length, first)),
        };
        let size = bytes.len() as u64;
        let entry = file_entry(block(n), options.extended, file_type, size, form, &area);
        put(&mut image, UDF_PARTITION + block(n), &entry);
        if let Some(first) = place {
            let at = (UDF_PARTITION + first) as usize * SECTOR;
            image[at..at + bytes.len()].copy_from_slice(&bytes);
        }
    }
    image
}

/// An image with a volume of UDF and a partition of `blocks` empty blocks.
///
/// The partition has the number that Windows gives it, 0x0BAD, so a reader
/// that takes the number of a map for its place in the list fails.
pub(crate) fn udf_volume(options: &Udf, blocks: u32) -> Vec<u8> {
    let last = UDF_PARTITION + blocks;
    let mut image = vec![0u8; (last as usize + 1) * SECTOR];
    for (sector, id) in [(16, b"BEA01"), (17, options.nsr), (18, b"TEA01")] {
        write_at(&mut image, sector * SECTOR + 1, id);
        image[sector * SECTOR + 6] = 1;
    }
    put(&mut image, 256, &anchor(256, UDF_MAIN, UDF_RESERVE));
    put(&mut image, last, &anchor(last, UDF_MAIN, UDF_RESERVE));
    for first in [UDF_MAIN, UDF_RESERVE] {
        let maps = [type1_map(0x0BAD)];
        put(
            &mut image,
            first,
            &logical_volume(first, 1, options.label, &maps, 0),
        );
        let pd = partition(first + 1, 2, 0x0BAD, UDF_PARTITION, blocks);
        put(&mut image, first + 1, &pd);
        put(&mut image, first + 2, &terminator(first + 2));
    }
    image
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
    let primary = Names {
        joliet: false,
        rock_ridge: options.rock_ridge,
    };
    let joliet = Names {
        joliet: true,
        rock_ridge: false,
    };
    let trees: Vec<Names> = std::iter::once(primary)
        .chain(options.joliet.then_some(joliet))
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
        let dot = match names.rock_ridge && dir.is_empty() {
            true => rr_root(0),
            false => Vec::new(),
        };
        let mut records = vec![
            record_bytes(own, own_len, 0x02, &[0], &dot),
            record_bytes(up, up_len, 0x02, &[1], &[]),
        ];
        for at in children.get(dir).map(Vec::as_slice).unwrap_or(&[]) {
            let item = &items[*at];
            let name = item.path().rsplit('/').next().unwrap();
            let su = names.system_use(item, name);
            let r = match item {
                Item::Dir(path) => {
                    let (e, l) = extents.get(&(t, *path)).copied().unwrap_or((0, 0));
                    record_bytes(e, l, 0x02, &names.id(*at, name, false), &su)
                }
                Item::File(_, bytes) => {
                    let (e, _) = files[*at];
                    record_bytes(e, bytes.len() as u32, 0, &names.id(*at, name, true), &su)
                }
                Item::Link(..) if names.rock_ridge => {
                    record_bytes(0, 0, 0, &names.id(*at, name, true), &su)
                }
                Item::Link(..) => continue,
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
