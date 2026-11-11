//! A file whose attributes do not fit one record: $ATTRIBUTE_LIST in the
//! base record, and extension records that hold the rest.
//!
//! The base record keeps $STANDARD_INFORMATION, the attribute list and the
//! first file name. The other attributes fill extension records in the
//! order of their type. Each extension record names the base record, and the
//! list names the record and the ID of each attribute of the file. A list
//! that the base record cannot hold goes into clusters of its own.

use crate::ntfs::record::{kind, reference, Attribute, Record, Value, RECORD_BYTES};
use crate::ntfs::runs::Run;

/// The bytes that the attributes of a record may take.
const SPACE: usize = RECORD_BYTES - 0x38 - 8;

/// The attributes of a record in the order that `Record::to_bytes` gives
/// them, so the place of each is its ID.
fn ordered(attributes: &[Attribute]) -> Vec<Attribute> {
    let mut a = attributes.to_vec();
    a.sort_by(|x, y| x.kind.cmp(&y.kind).then_with(|| x.name.cmp(&y.name)));
    a
}

fn size(a: &Attribute) -> usize {
    a.to_bytes(0).len()
}

/// One entry of an attribute list.
fn list_entry(a: &Attribute, record: u64, id: u16) -> Vec<u8> {
    let name: Vec<u8> = a
        .name
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let len = (0x1A + name.len() + 7) & !7;
    let mut e = vec![0u8; len];
    e[0..4].copy_from_slice(&a.kind.to_le_bytes());
    e[4..6].copy_from_slice(&(len as u16).to_le_bytes());
    e[6] = (name.len() / 2) as u8;
    e[7] = 0x1A;
    // The first VCN is 0, because no attribute here is split.
    e[0x10..0x18].copy_from_slice(&record.to_le_bytes());
    e[0x18..0x1A].copy_from_slice(&id.to_le_bytes());
    e[0x1A..0x1A + name.len()].copy_from_slice(&name);
    e
}

/// Split `base` into a base record and extension records. `next` gives the
/// number of each extension record, and `take` gives clusters for a list
/// that the base record cannot hold, with the bytes to write there.
pub(crate) fn split(
    base: Record,
    mut next: impl FnMut() -> Result<u32, String>,
    mut take: impl FnMut(u64) -> Result<Run, String>,
    cluster_bytes: u64,
    writes: &mut Vec<(u64, Vec<u8>)>,
) -> Result<Vec<Record>, String> {
    let base_ref = reference(base.number as u64, base.sequence);
    let all = ordered(&base.attributes);
    let first_name = all.iter().position(|a| a.kind == kind::FILE_NAME);
    let mut kept = Vec::new();
    let mut moved = Vec::new();
    for (i, a) in all.into_iter().enumerate() {
        if a.kind == kind::STANDARD_INFORMATION || Some(i) == first_name {
            kept.push(a);
        } else {
            moved.push(a);
        }
    }

    // Fill extension records, each up to its space.
    let mut extensions: Vec<Record> = Vec::new();
    let mut used = SPACE;
    for a in moved {
        let n = size(&a);
        if n > SPACE {
            return Err(format!(
                "an attribute of {} bytes of record {} does not fit any record",
                n, base.number
            ));
        }
        if used + n > SPACE {
            extensions.push(Record {
                number: next()?,
                sequence: 1,
                flags: base.flags & 0x0001,
                links: 0,
                base: base_ref,
                attributes: Vec::new(),
            });
            used = 0;
        }
        used += n;
        extensions.last_mut().unwrap().attributes.push(a);
    }

    // The list: each attribute of each record but the list itself, with the
    // record that holds it and its ID there.
    let mut entries: Vec<(u32, String, Vec<u8>)> = Vec::new();
    let with_list_placeholder = {
        let mut k = kept.clone();
        k.push(Attribute::resident(kind::ATTRIBUTE_LIST, "", Vec::new()));
        k
    };
    for (id, a) in ordered(&with_list_placeholder).iter().enumerate() {
        if a.kind != kind::ATTRIBUTE_LIST {
            entries.push((a.kind, a.name.clone(), list_entry(a, base_ref, id as u16)));
        }
    }
    for e in &extensions {
        let r = reference(e.number as u64, e.sequence);
        for (id, a) in ordered(&e.attributes).iter().enumerate() {
            entries.push((a.kind, a.name.clone(), list_entry(a, r, id as u16)));
        }
    }
    entries.sort_by(|x, y| x.0.cmp(&y.0).then_with(|| x.1.cmp(&y.1)));
    let list: Vec<u8> = entries.into_iter().flat_map(|e| e.2).collect();

    let kept_bytes: usize = kept.iter().map(size).sum();
    let resident = Attribute::resident(kind::ATTRIBUTE_LIST, "", list.clone());
    let list_attribute = if kept_bytes + size(&resident) <= SPACE {
        resident
    } else {
        let run = take(list.len() as u64)?;
        writes.push((run.lcn.unwrap() * cluster_bytes, list.clone()));
        Attribute {
            kind: kind::ATTRIBUTE_LIST,
            name: String::new(),
            value: Value::NonResident {
                runs: vec![run],
                size: list.len() as u64,
                initialized: list.len() as u64,
                cluster_bytes,
            },
        }
    };
    kept.push(list_attribute);
    let mut out = vec![Record {
        attributes: kept,
        ..base
    }];
    out.extend(extensions);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::record::{remove_fixups, FLAG_IN_USE};

    fn name(n: usize) -> Attribute {
        Attribute::resident(kind::FILE_NAME, "", vec![n as u8; 0x42 + 2 * 100])
    }

    fn big_file() -> Record {
        let mut attributes = vec![Attribute::resident(
            kind::STANDARD_INFORMATION,
            "",
            vec![0; 72],
        )];
        attributes.extend((0..8).map(name));
        attributes.push(Attribute::resident(kind::DATA, "", vec![7; 40]));
        Record {
            number: 100,
            sequence: 1,
            flags: FLAG_IN_USE,
            links: 8,
            base: 0,
            attributes,
        }
    }

    #[test]
    fn a_file_with_many_names_gets_extension_records_and_a_list() {
        assert!(big_file().to_bytes().is_err());
        let mut n = 200;
        let mut writes = Vec::new();
        let records = split(
            big_file(),
            || {
                n += 1;
                Ok(n)
            },
            |_| unreachable!("the list fits the base record"),
            4096,
            &mut writes,
        )
        .unwrap();
        assert!(records.len() >= 3);
        for r in &records {
            let bytes = remove_fixups(&r.to_bytes().unwrap()).unwrap();
            assert_eq!(&bytes[0..4], b"FILE");
        }
        let base = &records[0];
        assert_eq!(base.links, 8);
        assert_eq!(base.attributes.len(), 3);
        for e in &records[1..] {
            assert_eq!(e.base, reference(100, 1));
            assert_eq!(e.links, 0);
        }
        // The list names 9 attributes: SI, 8 names and the data.
        let list = base
            .attributes
            .iter()
            .find(|a| a.kind == kind::ATTRIBUTE_LIST)
            .unwrap();
        let Value::Resident { bytes, .. } = &list.value else {
            panic!("the list is resident")
        };
        let mut at = 0;
        let mut count = 0;
        let mut last_kind = 0;
        while at < bytes.len() {
            let k = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            assert!(k >= last_kind, "in the order of the type");
            last_kind = k;
            count += 1;
            at += u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]) as usize;
        }
        assert_eq!(count, 10);
    }

    #[test]
    fn a_list_too_large_for_the_base_record_goes_into_clusters() {
        let mut big = big_file();
        big.attributes.extend((8..60).map(name));
        let mut n = 200;
        let mut writes = Vec::new();
        let records = split(
            big,
            || {
                n += 1;
                Ok(n)
            },
            |bytes| {
                Ok(Run {
                    lcn: Some(999),
                    clusters: bytes.div_ceil(4096),
                })
            },
            4096,
            &mut writes,
        )
        .unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, 999 * 4096);
        let list = records[0]
            .attributes
            .iter()
            .find(|a| a.kind == kind::ATTRIBUTE_LIST)
            .unwrap();
        assert!(matches!(list.value, Value::NonResident { .. }));
    }
}
