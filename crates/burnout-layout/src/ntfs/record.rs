//! One record of the master file table: a header, attributes, and the
//! update sequence that guards each sector of it.
//!
//! The last two bytes of each sector of a record hold the update sequence
//! number, and the array near the start of the record keeps the bytes that
//! were there. A reader that finds a sector without the number knows that
//! the sector did not reach the disk. The record is built whole, and the
//! fixups go in last, when the record is turned into bytes.

use crate::ntfs::runs::{encode, Run};

/// The bytes of one record.
pub(crate) const RECORD_BYTES: usize = 1024;
/// The sector that the update sequence guards.
const SECTOR: usize = 512;
const USA_OFFSET: usize = 0x30;
const USA_COUNT: usize = 1 + RECORD_BYTES / SECTOR;
/// The first attribute, on the 8-byte boundary after the update sequence.
const FIRST_ATTRIBUTE: usize = (USA_OFFSET + 2 * USA_COUNT + 7) & !7;
const END_MARKER: u32 = 0xFFFF_FFFF;

pub(crate) const FLAG_IN_USE: u16 = 0x0001;
pub(crate) const FLAG_DIRECTORY: u16 = 0x0002;
/// The record holds an index that is not of file names, such as $Secure.
pub(crate) const FLAG_VIEW_INDEX: u16 = 0x0008;

/// The types of attribute, as `$AttrDef` lists them.
pub(crate) mod kind {
    pub const STANDARD_INFORMATION: u32 = 0x10;
    pub const ATTRIBUTE_LIST: u32 = 0x20;
    pub const FILE_NAME: u32 = 0x30;
    pub const OBJECT_ID: u32 = 0x40;
    pub const SECURITY_DESCRIPTOR: u32 = 0x50;
    pub const VOLUME_NAME: u32 = 0x60;
    pub const VOLUME_INFORMATION: u32 = 0x70;
    pub const DATA: u32 = 0x80;
    pub const INDEX_ROOT: u32 = 0x90;
    pub const INDEX_ALLOCATION: u32 = 0xA0;
    pub const BITMAP: u32 = 0xB0;
    pub const REPARSE_POINT: u32 = 0xC0;
    pub const EA_INFORMATION: u32 = 0xD0;
    pub const EA: u32 = 0xE0;
    pub const LOGGED_UTILITY_STREAM: u32 = 0x100;
}

/// Where the value of an attribute is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// In the record itself. `indexed` marks a value that an index of the
    /// directory also holds, which a file name is.
    Resident { bytes: Vec<u8>, indexed: bool },
    /// In clusters of the volume.
    NonResident {
        runs: Vec<Run>,
        /// The bytes of the value, and the bytes that hold data. The bytes
        /// past `initialized` read as zeros.
        size: u64,
        initialized: u64,
        cluster_bytes: u64,
    },
}

/// One attribute of a record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attribute {
    pub kind: u32,
    /// The name, such as `$I30` for the index of a directory, or empty.
    pub name: String,
    pub value: Value,
}

impl Attribute {
    pub(crate) fn resident(kind: u32, name: &str, bytes: Vec<u8>) -> Attribute {
        Attribute {
            kind,
            name: name.to_string(),
            value: Value::Resident {
                bytes,
                indexed: kind == kind::FILE_NAME,
            },
        }
    }

    /// The bytes of the attribute in a record, with this ID.
    pub(crate) fn to_bytes(&self, id: u16) -> Vec<u8> {
        let name: Vec<u8> = self
            .name
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let mut a = match &self.value {
            Value::Resident { bytes, indexed } => {
                let name_at = 0x18;
                let value_at = align8(name_at + name.len());
                let mut a = vec![0u8; value_at];
                a[0x08] = 0;
                a[0x0A..0x0C].copy_from_slice(&(name_at as u16).to_le_bytes());
                a[0x10..0x14].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
                a[0x14..0x16].copy_from_slice(&(value_at as u16).to_le_bytes());
                a[0x16] = *indexed as u8;
                a[name_at..name_at + name.len()].copy_from_slice(&name);
                a.extend_from_slice(bytes);
                a
            }
            Value::NonResident {
                runs,
                size,
                initialized,
                cluster_bytes,
            } => {
                let clusters: u64 = runs.iter().map(|r| r.clusters).sum();
                let name_at = 0x40;
                let runs_at = align8(name_at + name.len());
                let mut a = vec![0u8; runs_at];
                a[0x08] = 1;
                a[0x0A..0x0C].copy_from_slice(&(name_at as u16).to_le_bytes());
                // The first VCN is 0, and the last is one less than the count.
                let last_vcn = clusters.saturating_sub(1);
                a[0x18..0x20].copy_from_slice(&last_vcn.to_le_bytes());
                a[0x20..0x22].copy_from_slice(&(runs_at as u16).to_le_bytes());
                a[0x28..0x30].copy_from_slice(&(clusters * cluster_bytes).to_le_bytes());
                a[0x30..0x38].copy_from_slice(&size.to_le_bytes());
                a[0x38..0x40].copy_from_slice(&initialized.to_le_bytes());
                a[name_at..name_at + name.len()].copy_from_slice(&name);
                a.extend_from_slice(&encode(runs));
                a
            }
        };
        a.resize(align8(a.len()), 0);
        let length = a.len() as u32;
        a[0x00..0x04].copy_from_slice(&self.kind.to_le_bytes());
        a[0x04..0x08].copy_from_slice(&length.to_le_bytes());
        a[0x09] = self.name.encode_utf16().count() as u8;
        a[0x0E..0x10].copy_from_slice(&id.to_le_bytes());
        a
    }
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// One record of the master file table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub number: u32,
    pub sequence: u16,
    pub flags: u16,
    /// The count of file names in directories that name this record.
    pub links: u16,
    /// The record that this one extends, as a file reference, or 0.
    pub base: u64,
    /// In the order of their type, as NTFS requires.
    pub attributes: Vec<Attribute>,
}

/// A record does not hold its attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordFull {
    pub number: u32,
    pub needed: usize,
}

impl Record {
    /// The bytes of the record, with the fixups in place.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, RecordFull> {
        let mut attributes = self.attributes.clone();
        // NTFS keeps attributes in the order of their type, then of their
        // name. The sort is stable, so two of one type keep their order.
        attributes.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
        let mut body = Vec::new();
        for (id, a) in attributes.iter().enumerate() {
            body.extend_from_slice(&a.to_bytes(id as u16));
        }
        let used = FIRST_ATTRIBUTE + body.len() + 8;
        if used > RECORD_BYTES {
            return Err(RecordFull {
                number: self.number,
                needed: used,
            });
        }
        let mut r = vec![0u8; RECORD_BYTES];
        r[0..4].copy_from_slice(b"FILE");
        r[0x04..0x06].copy_from_slice(&(USA_OFFSET as u16).to_le_bytes());
        r[0x06..0x08].copy_from_slice(&(USA_COUNT as u16).to_le_bytes());
        r[0x10..0x12].copy_from_slice(&self.sequence.to_le_bytes());
        r[0x12..0x14].copy_from_slice(&self.links.to_le_bytes());
        r[0x14..0x16].copy_from_slice(&(FIRST_ATTRIBUTE as u16).to_le_bytes());
        r[0x16..0x18].copy_from_slice(&self.flags.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&(used as u32).to_le_bytes());
        r[0x1C..0x20].copy_from_slice(&(RECORD_BYTES as u32).to_le_bytes());
        r[0x20..0x28].copy_from_slice(&self.base.to_le_bytes());
        r[0x28..0x2A].copy_from_slice(&(attributes.len() as u16).to_le_bytes());
        r[0x2C..0x30].copy_from_slice(&self.number.to_le_bytes());
        r[FIRST_ATTRIBUTE..FIRST_ATTRIBUTE + body.len()].copy_from_slice(&body);
        let end = FIRST_ATTRIBUTE + body.len();
        r[end..end + 4].copy_from_slice(&END_MARKER.to_le_bytes());
        apply_fixups(&mut r, 1);
        Ok(r)
    }
}

/// A file reference: the record number in the low 48 bits, and the sequence
/// number of the record in the high 16.
pub(crate) fn reference(number: u64, sequence: u16) -> u64 {
    (number & 0xFFFF_FFFF_FFFF) | ((sequence as u64) << 48)
}

/// Put the update sequence number at the end of each sector of `block`, and
/// keep the bytes that were there in the array. The header of the block
/// gives the place and the size of the array.
pub(crate) fn apply_fixups(block: &mut [u8], usn: u16) {
    let at = u16::from_le_bytes([block[4], block[5]]) as usize;
    let count = u16::from_le_bytes([block[6], block[7]]) as usize;
    block[at..at + 2].copy_from_slice(&usn.to_le_bytes());
    for i in 1..count {
        let end = i * SECTOR;
        let (saved, tail) = (at + 2 * i, end - 2);
        let kept = [block[tail], block[tail + 1]];
        block[saved..saved + 2].copy_from_slice(&kept);
        block[tail..tail + 2].copy_from_slice(&usn.to_le_bytes());
    }
}

/// Undo `apply_fixups`, or `None` when a sector does not end with the
/// number, which is a torn write.
pub(crate) fn remove_fixups(block: &[u8]) -> Option<Vec<u8>> {
    let mut b = block.to_vec();
    let at = u16::from_le_bytes([b[4], b[5]]) as usize;
    let count = u16::from_le_bytes([b[6], b[7]]) as usize;
    let usn = [b[at], b[at + 1]];
    for i in 1..count {
        let tail = i * SECTOR - 2;
        if b.get(tail..tail + 2)? != usn {
            return None;
        }
        let saved = at + 2 * i;
        let kept = [b[saved], b[saved + 1]];
        b[tail..tail + 2].copy_from_slice(&kept);
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn u64_at(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    fn sample() -> Record {
        Record {
            number: 5,
            sequence: 5,
            flags: FLAG_IN_USE | FLAG_DIRECTORY,
            links: 1,
            base: 0,
            attributes: vec![
                Attribute {
                    kind: kind::DATA,
                    name: String::new(),
                    value: Value::NonResident {
                        runs: vec![Run {
                            lcn: Some(0x5634),
                            clusters: 0x18,
                        }],
                        size: 0x17_000,
                        initialized: 0x16_000,
                        cluster_bytes: 4096,
                    },
                },
                Attribute::resident(kind::STANDARD_INFORMATION, "", vec![7; 72]),
                Attribute::resident(kind::FILE_NAME, "", vec![9; 10]),
            ],
        }
    }

    #[test]
    fn a_record_has_its_header_and_its_attributes_in_the_order_of_their_type() {
        let r = remove_fixups(&sample().to_bytes().unwrap()).unwrap();
        assert_eq!(&r[0..4], b"FILE");
        assert_eq!(u16_at(&r, 0x04), 0x30);
        assert_eq!(u16_at(&r, 0x06), 3);
        assert_eq!(u16_at(&r, 0x10), 5);
        assert_eq!(u16_at(&r, 0x14), 0x38);
        assert_eq!(u16_at(&r, 0x16), 3);
        assert_eq!(u32_at(&r, 0x1C), 1024);
        assert_eq!(u16_at(&r, 0x28), 3);
        assert_eq!(u32_at(&r, 0x2C), 5);

        // $STANDARD_INFORMATION, resident, its value at 0x18.
        let a = 0x38;
        assert_eq!(u32_at(&r, a), 0x10);
        assert_eq!(u32_at(&r, a + 4), 0x18 + 72);
        assert_eq!(r[a + 8], 0);
        assert_eq!(u32_at(&r, a + 0x10), 72);
        assert_eq!(u16_at(&r, a + 0x14), 0x18);
        assert_eq!(u16_at(&r, a + 0x0E), 0, "the first ID");
        assert_eq!(&r[a + 0x18..a + 0x18 + 72], &[7; 72]);

        // $FILE_NAME, resident and indexed, padded to 8 bytes.
        let b = a + 0x18 + 72;
        assert_eq!(u32_at(&r, b), 0x30);
        assert_eq!(u32_at(&r, b + 4), 0x28);
        assert_eq!(r[b + 0x16], 1);

        // $DATA, non-resident, with its sizes and its run list.
        let c = b + 0x28;
        assert_eq!(u32_at(&r, c), 0x80);
        assert_eq!(r[c + 8], 1);
        assert_eq!(u64_at(&r, c + 0x18), 0x17, "the last VCN");
        assert_eq!(u16_at(&r, c + 0x20), 0x40);
        assert_eq!(u64_at(&r, c + 0x28), 0x18 * 4096);
        assert_eq!(u64_at(&r, c + 0x30), 0x17_000);
        assert_eq!(u64_at(&r, c + 0x38), 0x16_000);
        assert_eq!(&r[c + 0x40..c + 0x45], &[0x21, 0x18, 0x34, 0x56, 0]);
        let end = c + u32_at(&r, c + 4) as usize;
        assert_eq!(u32_at(&r, end), END_MARKER);
        assert_eq!(u32_at(&r, 0x18) as usize, end + 8);
    }

    #[test]
    fn a_named_attribute_puts_its_name_before_its_value() {
        let a = Attribute::resident(kind::INDEX_ROOT, "$I30", vec![1, 2, 3]);
        let b = a.to_bytes(4);
        assert_eq!(b[0x09], 4);
        assert_eq!(u16_at(&b, 0x0A), 0x18);
        let name: Vec<u8> = "$I30"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(&b[0x18..0x20], &name[..]);
        assert_eq!(u16_at(&b, 0x14), 0x20);
        assert_eq!(&b[0x20..0x23], &[1, 2, 3]);
        assert_eq!(u16_at(&b, 0x0E), 4);
    }

    #[test]
    fn the_fixups_guard_each_sector_and_come_off_again() {
        let mut r = sample().to_bytes().unwrap();
        assert_eq!(u16_at(&r, 0x30), 1);
        assert_eq!(u16_at(&r, 510), 1);
        assert_eq!(u16_at(&r, 1022), 1);
        let clean = remove_fixups(&r).unwrap();
        assert_eq!(u16_at(&clean, 510), 0);
        // A sector that did not reach the disk.
        r[1022] = 9;
        assert_eq!(remove_fixups(&r), None);
    }

    #[test]
    fn a_record_that_does_not_hold_its_attributes_says_so() {
        let mut big = sample();
        big.attributes
            .push(Attribute::resident(kind::DATA, "x", vec![0; 900]));
        let e = big.to_bytes().unwrap_err();
        assert_eq!(e.number, 5);
        assert!(e.needed > RECORD_BYTES);
    }

    #[test]
    fn a_reference_holds_the_number_and_the_sequence() {
        assert_eq!(reference(5, 5), 0x0005_0000_0000_0005);
        assert_eq!(reference(1 << 48 | 7, 1), 0x0001_0000_0000_0007);
    }
}
