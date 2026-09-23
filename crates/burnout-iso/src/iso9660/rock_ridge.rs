//! Rock Ridge, which gives the names, the links and the modes of a POSIX
//! system to the primary tree of ISO 9660.
//!
//! Rock Ridge rides on the System Use Sharing Protocol: each record of a
//! directory carries entries after its name, and an entry can point at a
//! continuation area in another block. The root directory says in its first
//! record that the protocol is in use, and how many bytes to skip before the
//! entries of each other record.

use std::io::{Read, Seek};

use burnout_core::Result;

use super::descriptors::Root;
use super::directory::records;
use crate::image::{Image, SECTOR};
use crate::node::tree_name;

/// The most continuation areas that one record may chain.
const MOST_AREAS: usize = 32;

/// The identifiers that an ER entry gives for Rock Ridge, at its versions.
const RRIP: [&[u8]; 3] = [b"RRIP_1991A", b"IEEE_P1282", b"IEEE_1282"];

/// The flags of a name, and of a component of a link.
const CONTINUE: u8 = 0x01;
const CURRENT: u8 = 0x02;
const PARENT: u8 = 0x04;
const ROOT: u8 = 0x08;

/// What the entries of one record say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Entries {
    name: Vec<u8>,
    has_name: bool,
    /// The components of a link: their flags and their bytes.
    components: Vec<(u8, Vec<u8>)>,
    has_link: bool,
    /// The block of a directory that was moved, for a record that stands in
    /// for it.
    pub child_link: Option<u32>,
    /// The record of a directory that was moved. The tree holds it where its
    /// child link is, and not here.
    pub relocated: bool,
    /// An entry that says the data is compressed with zisofs.
    pub compressed: bool,
    /// An entry that says the data is sparse.
    pub sparse: bool,
    /// Whether an entry of Rock Ridge came at all.
    pub rock_ridge: bool,
    /// The bytes to skip in each system use area, from an SP entry.
    pub skip: Option<u8>,
    /// The POSIX mode, from a PX entry.
    pub mode: Option<u32>,
}

impl Entries {
    /// The name that Rock Ridge gives, or `None` when it gives none.
    pub(crate) fn name(&self) -> Option<std::result::Result<String, String>> {
        self.has_name.then(|| {
            String::from_utf8(self.name.clone())
                .map_err(|_| "a Rock Ridge name is not UTF-8".to_string())
                .and_then(tree_name)
        })
    }

    /// The path that a link names, or `None` when this is not a link.
    pub(crate) fn link(&self) -> Option<String> {
        if !self.has_link {
            return None;
        }
        let mut path = String::new();
        let mut joined = true;
        for (flags, bytes) in &self.components {
            if !joined && !path.ends_with('/') {
                path.push('/');
            }
            match *flags & !CONTINUE {
                f if f & ROOT != 0 => path.push('/'),
                f if f & PARENT != 0 => path.push_str(".."),
                f if f & CURRENT != 0 => path.push('.'),
                _ => path.push_str(&String::from_utf8_lossy(bytes)),
            }
            // A component with CONTINUE goes on in the next one, with no `/`.
            joined = *flags & CONTINUE != 0;
        }
        Some(path)
    }

    /// Take the entries of one area, and give the continuation area that it
    /// names, if any.
    fn feed(&mut self, area: &[u8]) -> std::result::Result<Option<Continuation>, String> {
        let mut next = None;
        let mut at = 0;
        while at + 4 <= area.len() {
            let signature = &area[at..at + 2];
            let length = area[at + 2] as usize;
            if length < 4 || at + length > area.len() {
                // A signature is two letters, so a zero byte where an entry
                // starts is padding, and it ends the area. hdiutil pads a
                // record to an even length with one zero byte, and can put
                // a padding entry after it. Anything else is a fault.
                if signature[0] == 0 {
                    break;
                }
                return Err(format!(
                    "the entry {:?} of the system use area has a length of {length}, which does not fit",
                    String::from_utf8_lossy(signature)
                ));
            }
            let data = &area[at + 4..at + length];
            match signature {
                b"SP" if data.len() >= 3 && data[..2] == [0xBE, 0xEF] => self.skip = Some(data[2]),
                b"CE" if data.len() >= 24 => {
                    next = Some(Continuation {
                        block: le32(&data[0..4]),
                        offset: le32(&data[8..12]),
                        length: le32(&data[16..20]),
                    })
                }
                b"ER" if data.len() >= 4 => {
                    let id = data[0] as usize;
                    if RRIP.iter().any(|r| data.get(4..4 + id) == Some(r)) {
                        self.rock_ridge = true;
                    }
                }
                b"ST" => break,
                b"NM" if !data.is_empty() => {
                    self.rock_ridge = true;
                    // A name that means the directory or the one above adds
                    // nothing to the tree.
                    if data[0] & (CURRENT | PARENT) == 0 {
                        self.name.extend_from_slice(&data[1..]);
                        self.has_name = true;
                    }
                }
                b"SL" if !data.is_empty() => {
                    self.rock_ridge = true;
                    self.has_link = true;
                    let mut c = 1;
                    while c + 2 <= data.len() {
                        let (flags, n) = (data[c], data[c + 1] as usize);
                        let bytes = data
                            .get(c + 2..c + 2 + n)
                            .ok_or("a link component does not fit")?;
                        self.components.push((flags, bytes.to_vec()));
                        c += 2 + n;
                    }
                }
                b"CL" if data.len() >= 4 => self.child_link = Some(le32(&data[0..4])),
                b"RE" => self.relocated = true,
                b"ZF" => self.compressed = true,
                b"SF" => self.sparse = true,
                b"PX" if data.len() >= 4 => {
                    self.rock_ridge = true;
                    self.mode = Some(le32(&data[0..4]));
                }
                b"TF" | b"RR" | b"PN" | b"PL" => self.rock_ridge = true,
                _ => {}
            }
            at += length;
        }
        Ok(next)
    }
}

/// An area of more entries, in another block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Continuation {
    block: u32,
    offset: u32,
    length: u32,
}

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

/// The entries of one record, with every continuation area that it chains.
pub(crate) fn read_entries<R: Read + Seek>(
    image: &mut Image<R>,
    system_use: &[u8],
    skip: usize,
    what: &str,
) -> Result<Entries> {
    let mut entries = Entries::default();
    let fault =
        |image: &Image<R>, e: String| image.fault(format!("the Rock Ridge entries of {what}: {e}"));
    let mut next = entries
        .feed(system_use.get(skip..).unwrap_or(&[]))
        .map_err(|e| fault(image, e))?;
    let mut areas = 0;
    while let Some(c) = next {
        areas += 1;
        if areas > MOST_AREAS {
            return Err(fault(
                image,
                format!("more than {MOST_AREAS} continuation areas"),
            ));
        }
        if c.offset as u64 + c.length as u64 > SECTOR {
            return Err(fault(
                image,
                "a continuation area crosses the end of its block".to_string(),
            ));
        }
        let area = image.read_at(
            c.block as u64 * SECTOR + c.offset as u64,
            c.length as usize,
            "a continuation area of Rock Ridge",
        )?;
        next = entries.feed(&area).map_err(|e| fault(image, e))?;
    }
    Ok(entries)
}

/// What the root of a tree says of Rock Ridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RockRidge {
    /// The bytes to skip in each system use area.
    pub skip: u8,
    /// Whether the records of the root give names. hdiutil writes Rock
    /// Ridge with modes and times and no names, and then its tree holds
    /// only the short names of ISO 9660.
    pub names: bool,
}

/// Whether the tree at `root` carries Rock Ridge, and how.
///
/// The first record of the root directory holds an SP entry when the tree
/// uses the protocol, and the entries of Rock Ridge say which extension it
/// is.
pub(crate) fn detect<R: Read + Seek>(
    image: &mut Image<R>,
    root: Root,
) -> Result<Option<RockRidge>> {
    let first = image.read_at(
        root.extent as u64 * SECTOR,
        SECTOR as usize,
        "the root directory",
    )?;
    let records = records(&first).map_err(|e| image.fault(format!("the root directory: {e}")))?;
    let Some(dot) = records.iter().find(|r| r.is_self()) else {
        return Ok(None);
    };
    let entries = read_entries(image, &dot.system_use, 0, "the root directory")?;
    let (Some(skip), true) = (entries.skip, entries.rock_ridge) else {
        return Ok(None);
    };
    let mut names = false;
    for r in records.iter().filter(|r| !r.is_self() && !r.is_parent()) {
        let entries = read_entries(image, &r.system_use, skip as usize, "the root directory")?;
        if entries.name().is_some() {
            names = true;
            break;
        }
    }
    Ok(Some(RockRidge { skip, names }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::read_descriptors;
    use crate::testing::{iso9660, susp as entry, Iso, Item};
    use std::io::Cursor;

    fn fed(area: &[u8]) -> Entries {
        let mut e = Entries::default();
        e.feed(area).unwrap();
        e
    }

    #[test]
    fn a_name_in_parts_is_one_name() {
        let mut area = entry(b"NM", &[CONTINUE, b'a', b'b']);
        area.extend(entry(b"NM", b"\0cd.txt"));
        assert_eq!(fed(&area).name().unwrap().unwrap(), "abcd.txt");
    }

    #[test]
    fn a_record_with_no_name_entry_gives_no_name() {
        assert!(fed(&entry(b"PX", &[0; 32])).name().is_none());
    }

    #[test]
    fn a_link_joins_its_components_with_slashes() {
        let area = entry(
            b"SL",
            &[0, ROOT, 0, 0, 3, b'e', b't', b'c', 0, 2, b'o', b's'],
        );
        assert_eq!(fed(&area).link().unwrap(), "/etc/os");
        let area = entry(b"SL", &[0, PARENT, 0, 0, 4, b'b', b'o', b'o', b't']);
        assert_eq!(fed(&area).link().unwrap(), "../boot");
        let area = entry(b"SL", &[0, CURRENT, 0]);
        assert_eq!(fed(&area).link().unwrap(), ".");
    }

    #[test]
    fn a_component_that_continues_has_no_slash_after_it() {
        let area = entry(b"SL", &[0, CONTINUE, 2, b'a', b'b', 0, 2, b'c', b'd']);
        assert_eq!(fed(&area).link().unwrap(), "abcd");
    }

    #[test]
    fn a_link_in_two_entries_is_one_link() {
        let mut area = entry(b"SL", &[CONTINUE, 0, 3, b'u', b's', b'r']);
        area.extend(entry(b"SL", &[0, 0, 3, b'b', b'i', b'n']));
        assert_eq!(fed(&area).link().unwrap(), "usr/bin");
    }

    #[test]
    fn the_protocol_entry_gives_the_bytes_to_skip() {
        let e = fed(&entry(b"SP", &[0xBE, 0xEF, 3]));
        assert_eq!(e.skip, Some(3));
        assert!(!e.rock_ridge);
        assert_eq!(fed(&entry(b"SP", &[0xAA, 0xEF, 3])).skip, None);
    }

    #[test]
    fn an_extension_reference_names_rock_ridge() {
        for id in RRIP {
            let mut data = vec![id.len() as u8, 0, 0, 1];
            data.extend_from_slice(id);
            assert!(fed(&entry(b"ER", &data)).rock_ridge);
        }
        let mut other = vec![3, 0, 0, 1];
        other.extend_from_slice(b"XYZ");
        assert!(!fed(&entry(b"ER", &other)).rock_ridge);
    }

    #[test]
    fn a_continuation_entry_names_its_area() {
        let mut data = Vec::new();
        for value in [40u32, 100, 60] {
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&value.to_be_bytes());
        }
        let mut e = Entries::default();
        let next = e.feed(&entry(b"CE", &data)).unwrap();
        assert_eq!(
            next,
            Some(Continuation {
                block: 40,
                offset: 100,
                length: 60
            })
        );
    }

    #[test]
    fn relocation_and_compression_are_seen() {
        let mut data = Vec::new();
        data.extend_from_slice(&77u32.to_le_bytes());
        data.extend_from_slice(&77u32.to_be_bytes());
        let e = fed(&entry(b"CL", &data));
        assert_eq!(e.child_link, Some(77));
        assert!(fed(&entry(b"RE", &[])).relocated);
        assert!(fed(&entry(b"ZF", &[0; 12])).compressed);
    }

    #[test]
    fn an_entry_that_does_not_fit_its_area_is_refused() {
        let mut area = entry(b"NM", b"\0name");
        area[2] = 200;
        let e = Entries::default().feed(&area).unwrap_err();
        assert!(e.contains("\"NM\""), "{e}");
    }

    #[test]
    fn a_name_that_a_tree_cannot_hold_is_refused() {
        for bad in ["a/b", ".", ".."] {
            let e = fed(&entry(b"NM", &[b"\0", bad.as_bytes()].concat()));
            assert!(e.name().unwrap().is_err(), "{bad}");
        }
        assert!(fed(&entry(b"NM", b"\0\xff")).name().unwrap().is_err());
    }

    #[test]
    fn the_root_says_whether_the_tree_carries_rock_ridge() {
        let found = |options: Iso| {
            let image = iso9660(&[Item::File("a.txt", b"x")], options);
            let mut image = Image::new(Cursor::new(image), "test.iso").unwrap();
            let d = read_descriptors(&mut image).unwrap().unwrap();
            detect(&mut image, d.primary.root).unwrap()
        };
        assert_eq!(found(Iso::default()), None);
        let rock_ridge = Iso {
            rock_ridge: true,
            ..Iso::default()
        };
        let names = RockRidge {
            skip: 0,
            names: true,
        };
        assert_eq!(found(rock_ridge), Some(names));
        let nameless = Iso {
            nameless: true,
            ..rock_ridge
        };
        let no_names = RockRidge {
            skip: 0,
            names: false,
        };
        assert_eq!(found(nameless), Some(no_names));
    }

    #[test]
    fn zero_padding_ends_an_area() {
        let mut area = entry(b"NM", b"\0ok");
        area.extend([0, 0, 0, 0, 0]);
        assert_eq!(fed(&area).name().unwrap().unwrap(), "ok");
    }

    #[test]
    fn a_zero_byte_before_a_padding_entry_ends_an_area() {
        // The end of a record that hdiutil wrote: a time entry, one byte to
        // make the record even, and a padding entry of 32 bytes.
        let mut area = entry(b"NM", b"\0file.txt");
        area.extend(entry(b"TF", &[0x0F; 29]));
        area.push(0);
        area.extend(entry(b"PD", &[0; 28]));
        assert_eq!(fed(&area).name().unwrap().unwrap(), "file.txt");
    }
}
