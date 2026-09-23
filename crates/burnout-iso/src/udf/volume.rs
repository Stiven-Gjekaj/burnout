//! The volume of UDF in an image: the sequence that says UDF is there, the
//! anchor, the volume descriptors and the partition.
//!
//! The anchor is at block 256, and a copy is at the last block. It points at
//! the main sequence of volume descriptors and at a reserve copy of it. The
//! sequence gives the logical volume, which says where the file set is, and
//! the partitions that the blocks of the file set count from.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use burnout_core::{Error, Result};

use super::tag::{name, read_tag, ANCHOR, LOGICAL_VOLUME, PARTITION, POINTER, TERMINATING};
use super::text::dstring;
use super::{u16_at, u32_at};
use crate::image::{Image, SECTOR};

/// The block of the anchor. A copy is at the last block of the image.
const ANCHOR_BLOCK: u64 = 256;

/// The most volume descriptor pointers that one sequence may chain.
const MOST_POINTERS: usize = 16;

/// The one size of a block that Burnout reads: the sector of a disc.
const BLOCK: u32 = 2048;

/// Where a block is: its number in a partition, and the partition, as the
/// partition maps count them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Address {
    pub block: u32,
    pub partition: u16,
}

/// Where a partition starts in the image, and its length in blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Partition {
    start: u32,
    length: u32,
}

/// The volume of UDF in an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Volume {
    pub label: String,
    /// The partitions in the order of their maps.
    partitions: Vec<Partition>,
    /// Where the file set descriptor is.
    pub file_set: Address,
}

impl Volume {
    /// The byte of the image at which `length` bytes from `at` start. Each of
    /// the bytes must be inside the partition.
    pub(crate) fn locate(&self, at: Address, length: u64) -> std::result::Result<u64, String> {
        let p = self.partitions.get(at.partition as usize).ok_or_else(|| {
            format!(
                "a block is in partition {}, and the volume has {}",
                at.partition,
                self.partitions.len()
            )
        })?;
        if at.block as u64 + length.div_ceil(SECTOR) > p.length as u64 {
            return Err(format!(
                "{length} bytes from block {} of partition {} run past its end at block {}",
                at.block, at.partition, p.length
            ));
        }
        Ok((p.start as u64 + at.block as u64) * SECTOR)
    }
}

/// A run of sectors that the volume descriptors give: its bytes, and its
/// first sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    length: u32,
    location: u32,
}

fn extent_at(bytes: &[u8], at: usize) -> Extent {
    Extent {
        length: u32_at(bytes, at),
        location: u32_at(bytes, at + 4),
    }
}

/// The volume of UDF in the image, or `None` when the image holds no UDF.
pub(crate) fn find_volume<R: Read + Seek>(image: &mut Image<R>) -> Result<Option<Volume>> {
    if !recognized(image)? {
        return Ok(None);
    }
    let (main, reserve) = anchor(image)?;
    match sequence(image, main) {
        Ok(volume) => Ok(Some(volume)),
        Err(main) => match sequence(image, reserve) {
            Ok(volume) => Ok(Some(volume)),
            Err(reserve) => Err(image.fault(format!(
                "the main volume descriptor sequence of UDF: {}; and its reserve copy: {}",
                detail(main),
                detail(reserve)
            ))),
        },
    }
}

/// What an error says, without the name of the image that it starts with.
fn detail(e: Error) -> String {
    match e {
        Error::Image { detail, .. } => detail,
        other => other.to_string(),
    }
}

/// Whether the volume recognition sequence names UDF: an NSR02 or an NSR03
/// descriptor before the sequence ends.
fn recognized<R: Read + Seek>(image: &mut Image<R>) -> Result<bool> {
    for sector in 16..ANCHOR_BLOCK {
        if (sector + 1) * SECTOR > image.length() {
            break;
        }
        let d = image.read_at(sector * SECTOR, 7, "the volume recognition sequence")?;
        match &d[1..6] {
            b"NSR02" | b"NSR03" => return Ok(true),
            b"CD001" | b"BEA01" | b"BOOT2" | b"CDW02" => {}
            // TEA01 ends the sequence, and so does a sector that is not a
            // descriptor of it.
            _ => break,
        }
    }
    Ok(false)
}

/// The main sequence and its reserve copy, from the first anchor that is
/// valid.
fn anchor<R: Read + Seek>(image: &mut Image<R>) -> Result<(Extent, Extent)> {
    let sectors = image.length() / SECTOR;
    let places = [
        Some(ANCHOR_BLOCK),
        sectors.checked_sub(1),
        sectors.checked_sub(1 + ANCHOR_BLOCK),
    ];
    let mut faults = Vec::new();
    for at in places.into_iter().flatten() {
        if (at + 1) * SECTOR > image.length() {
            faults.push(format!("block {at} is past the end of the image"));
            continue;
        }
        let d = image.sector(at, "an anchor of UDF")?;
        match read_tag(&d, at as u32) {
            Ok(tag) if tag.id == ANCHOR => return Ok((extent_at(&d, 16), extent_at(&d, 24))),
            Ok(tag) => faults.push(format!("block {at} holds {}", name(tag.id))),
            Err(e) => faults.push(e),
        }
    }
    Err(image.fault(format!(
        "the image says that it holds UDF, and no anchor of UDF is valid: {}",
        faults.join("; ")
    )))
}

/// A descriptor, and its sequence number. Of two descriptors of one thing,
/// the higher number prevails.
type Numbered = (u32, Vec<u8>);

fn keep(slot: &mut Option<Numbered>, number: u32, bytes: Vec<u8>) {
    if slot.as_ref().map_or(true, |(kept, _)| number >= *kept) {
        *slot = Some((number, bytes));
    }
}

/// The volume that one volume descriptor sequence gives.
fn sequence<R: Read + Seek>(image: &mut Image<R>, first: Extent) -> Result<Volume> {
    let mut logical: Option<Numbered> = None;
    let mut partitions: BTreeMap<u16, Option<Numbered>> = BTreeMap::new();
    let mut extent = first;
    let mut pointers = 0;
    'extents: loop {
        for n in 0..(extent.length as u64 / SECTOR) {
            let at = extent.location as u64 + n;
            let d = image.sector(at, "a volume descriptor of UDF")?;
            // A sector that was never written ends a sequence, as the
            // terminating descriptor does.
            if d[..16].iter().all(|b| *b == 0) {
                break 'extents;
            }
            let tag = read_tag(&d, at as u32).map_err(|e| image.fault(e))?;
            let number = u32_at(&d, 16);
            match tag.id {
                TERMINATING => break 'extents,
                POINTER => {
                    pointers += 1;
                    if pointers > MOST_POINTERS {
                        return Err(image.fault(format!(
                            "the sequence chains more than {MOST_POINTERS} pointers"
                        )));
                    }
                    extent = extent_at(&d, 20);
                    continue 'extents;
                }
                LOGICAL_VOLUME => keep(&mut logical, number, d),
                PARTITION => keep(partitions.entry(u16_at(&d, 22)).or_default(), number, d),
                _ => {}
            }
        }
        break;
    }
    let Some((_, logical)) = logical else {
        return Err(image.fault("the sequence holds no logical volume descriptor"));
    };
    volume(image, &logical, &partitions)
}

/// The volume that a logical volume descriptor gives, with the partitions
/// that its maps name.
fn volume<R: Read + Seek>(
    image: &Image<R>,
    logical: &[u8],
    partitions: &BTreeMap<u16, Option<Numbered>>,
) -> Result<Volume> {
    let block = u32_at(logical, 212);
    if block != BLOCK {
        return Err(image.fault(format!(
            "the logical volume has blocks of {block} bytes, and Burnout reads blocks of {BLOCK}"
        )));
    }
    let label = dstring(&logical[84..212])
        .map_err(|e| image.fault(format!("the label of the volume: {e}")))?;
    let table = u32_at(logical, 264) as usize;
    let count = u32_at(logical, 268) as usize;
    let maps = logical.get(440..440 + table).ok_or_else(|| {
        image.fault(format!(
            "the partition maps take {table} bytes, which do not fit their descriptor"
        ))
    })?;

    let mut found = Vec::new();
    let mut at = 0;
    while found.len() < count {
        let n = found.len();
        let map = match maps.get(at..at + 2).map(|h| h[1] as usize) {
            Some(length) if length >= 2 => maps.get(at..at + length),
            _ => None,
        }
        .ok_or_else(|| image.fault(format!("partition map {n} does not fit the table")))?;
        match (map[0], map.len()) {
            (1, 6) => {
                let number = u16_at(map, 4);
                let Some(Some((_, pd))) = partitions.get(&number) else {
                    return Err(image.fault(format!(
                        "partition map {n} names partition {number}, and the sequence does not describe it"
                    )));
                };
                let contents = identifier(&pd[24..56]);
                if contents != b"+NSR02" && contents != b"+NSR03" {
                    return Err(image.fault(format!(
                        "partition {number} holds {:?}, and not a file set of UDF",
                        String::from_utf8_lossy(contents)
                    )));
                }
                found.push(Partition {
                    start: u32_at(pd, 188),
                    length: u32_at(pd, 192),
                });
            }
            (2, 64) => {
                let what = match identifier(&map[4..36]) {
                    b"*UDF Virtual Partition" => "a virtual partition",
                    b"*UDF Sparable Partition" => "a sparable partition",
                    b"*UDF Metadata Partition" => "a metadata partition",
                    _ => "a partition map of type 2",
                };
                return Err(image.fault(format!(
                    "the volume uses {what}, which Burnout does not read"
                )));
            }
            (kind, length) => {
                return Err(image.fault(format!(
                    "partition map {n} has type {kind} and {length} bytes, which UDF does not define"
                )));
            }
        }
        at += map.len();
    }
    let file_set = Address {
        block: u32_at(logical, 252),
        partition: u16_at(logical, 256),
    };
    if file_set.partition as usize >= found.len() {
        return Err(image.fault(format!(
            "the file set is in partition {}, and the volume has {}",
            file_set.partition,
            found.len()
        )));
    }
    Ok(Volume {
        label,
        partitions: found,
        file_set,
    })
}

/// The identifier of an entity: the 23 bytes after its flags, without the
/// zeros at the end.
fn identifier(entity: &[u8]) -> &[u8] {
    let id = &entity[1..24];
    let end = id.iter().position(|b| *b == 0).unwrap_or(id.len());
    &id[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        anchor as anchor_at, iso9660, logical_volume, partition, pointer, put, retag, terminator,
        type1_map, type2_map, udf_volume, Iso, Udf, UDF_MAIN, UDF_PARTITION, UDF_RESERVE,
    };
    use std::io::Cursor;

    fn find(image: Vec<u8>) -> Result<Option<Volume>> {
        find_volume(&mut Image::new(Cursor::new(image), "test.iso").unwrap())
    }

    /// An image whose main sequence is these descriptors, from its first
    /// block on. The reserve copy stays as the builder wrote it.
    fn with_main(descriptors: &[Vec<u8>]) -> Vec<u8> {
        let mut image = udf_volume(&Udf::default(), 10);
        write_sequence(&mut image, UDF_MAIN, descriptors);
        image
    }

    /// An image whose main sequence and reserve copy both hold a logical
    /// volume, a partition and a terminator, after `change` has changed the
    /// first two. A fault in both copies is a fault of the volume.
    fn with_both(change: impl Fn(&mut Vec<u8>, &mut Vec<u8>)) -> Vec<u8> {
        let mut image = udf_volume(&Udf::default(), 10);
        for first in [UDF_MAIN, UDF_RESERVE] {
            let mut lvd = logical_volume(first, 1, "T", &[type1_map(PART)], 0);
            let mut pd = partition(first + 1, 2, PART, UDF_PARTITION, 10);
            change(&mut lvd, &mut pd);
            retag(&mut lvd);
            retag(&mut pd);
            write_sequence(&mut image, first, &[lvd, pd, terminator(first + 2)]);
        }
        image
    }

    fn write_sequence(image: &mut [u8], first: u32, descriptors: &[Vec<u8>]) {
        for n in 0..16 {
            put(image, first + n, &[]);
        }
        for (n, d) in descriptors.iter().enumerate() {
            put(image, first + n as u32, d);
        }
    }

    fn refused(image: Vec<u8>) -> String {
        find(image).unwrap_err().to_string()
    }

    const PART: u16 = 0x0BAD;

    #[test]
    fn the_volume_gives_its_label_its_partition_and_its_file_set() {
        let v = find(udf_volume(&Udf::default(), 10)).unwrap().unwrap();
        assert_eq!(v.label, "TEST");
        let at = |block, partition| Address { block, partition };
        assert_eq!(v.file_set, at(0, 0));
        let byte = (UDF_PARTITION as u64 + 3) * 2048;
        assert_eq!(v.locate(at(3, 0), 2048).unwrap(), byte);
    }

    #[test]
    fn a_run_past_the_end_of_its_partition_is_refused() {
        let v = find(udf_volume(&Udf::default(), 10)).unwrap().unwrap();
        let at = |block, partition| Address { block, partition };
        assert!(v.locate(at(9, 0), 2048).is_ok());
        let e = v.locate(at(9, 0), 2049).unwrap_err();
        assert!(e.contains("run past its end at block 10"), "{e}");
        assert!(v.locate(at(0, 1), 1).unwrap_err().contains("partition 1"));
    }

    #[test]
    fn an_image_without_nsr_holds_no_udf() {
        assert_eq!(find(iso9660(&[], Iso::default())).unwrap(), None);
        assert_eq!(find(vec![0; 300 * 2048]).unwrap(), None);
        assert_eq!(find(vec![0; 1000]).unwrap(), None);
    }

    #[test]
    fn nsr03_names_udf_too() {
        let options = Udf {
            nsr: b"NSR03",
            ..Udf::default()
        };
        assert!(find(udf_volume(&options, 10)).unwrap().is_some());
    }

    #[test]
    fn a_broken_first_anchor_leaves_the_one_at_the_last_block() {
        let mut image = udf_volume(&Udf::default(), 10);
        put(&mut image, 256, &[]);
        assert!(find(image).unwrap().is_some());
    }

    #[test]
    fn an_image_with_no_valid_anchor_is_refused() {
        let mut image = udf_volume(&Udf::default(), 10);
        let last = image.len() as u32 / 2048 - 1;
        put(&mut image, 256, &[]);
        let moved = anchor_at(last + 1, UDF_MAIN, UDF_RESERVE);
        put(&mut image, last, &moved);
        let e = refused(image);
        assert!(e.contains("no anchor of UDF is valid"), "{e}");
        assert!(e.contains("says that it is at block"), "{e}");
    }

    #[test]
    fn a_broken_main_sequence_leaves_the_reserve() {
        let mut image = udf_volume(&Udf::default(), 10);
        let reserve = logical_volume(UDF_RESERVE, 1, "RESERVE", &[type1_map(PART)], 0);
        put(&mut image, UDF_RESERVE, &reserve);
        image[UDF_MAIN as usize * 2048 + 100] ^= 0xFF;
        assert_eq!(find(image).unwrap().unwrap().label, "RESERVE");
    }

    #[test]
    fn two_broken_sequences_are_refused_with_both_faults() {
        let mut image = udf_volume(&Udf::default(), 10);
        image[UDF_MAIN as usize * 2048 + 100] ^= 0xFF;
        put(&mut image, UDF_RESERVE, &[]);
        let e = refused(image);
        assert!(e.contains("the CRC of a logical volume descriptor"), "{e}");
        let reserve = "its reserve copy: the sequence holds no logical volume";
        assert!(e.contains(reserve), "{e}");
    }

    #[test]
    fn the_descriptor_with_the_higher_number_prevails() {
        let image = with_main(&[
            logical_volume(UDF_MAIN, 5, "NEW", &[type1_map(PART)], 0),
            logical_volume(UDF_MAIN + 1, 1, "OLD", &[type1_map(PART)], 0),
            partition(UDF_MAIN + 2, 2, PART, UDF_PARTITION, 10),
            terminator(UDF_MAIN + 3),
        ]);
        assert_eq!(find(image).unwrap().unwrap().label, "NEW");
    }

    #[test]
    fn a_pointer_goes_on_to_the_next_part_of_the_sequence() {
        let next = UDF_PARTITION + 4;
        let mut image = with_main(&[pointer(UDF_MAIN, 1, next, 3)]);
        let far = logical_volume(next, 2, "FAR", &[type1_map(PART)], 0);
        put(&mut image, next, &far);
        let pd = partition(next + 1, 3, PART, UDF_PARTITION, 10);
        put(&mut image, next + 1, &pd);
        put(&mut image, next + 2, &terminator(next + 2));
        assert_eq!(find(image).unwrap().unwrap().label, "FAR");
    }

    #[test]
    fn a_pointer_to_itself_is_refused() {
        let mut image = udf_volume(&Udf::default(), 10);
        for first in [UDF_MAIN, UDF_RESERVE] {
            write_sequence(&mut image, first, &[pointer(first, 1, first, 1)]);
        }
        let e = refused(image);
        assert!(e.contains("more than 16 pointers"), "{e}");
    }

    #[test]
    fn a_partition_map_of_type_2_is_refused_by_name() {
        for (id, what) in [
            ("*UDF Virtual Partition", "a virtual partition"),
            ("*UDF Sparable Partition", "a sparable partition"),
            ("*UDF Metadata Partition", "a metadata partition"),
        ] {
            let e = refused(with_both(|lvd, _| {
                let first = u32::from_le_bytes(lvd[12..16].try_into().unwrap());
                *lvd = logical_volume(first, 1, "T", &[type1_map(PART), type2_map(id)], 0);
            }));
            assert!(e.contains(&format!("uses {what}, which Burnout")), "{e}");
        }
    }

    #[test]
    fn a_map_of_a_partition_that_the_sequence_does_not_describe_is_refused() {
        let e = refused(with_both(|_, pd| {
            pd[22..24].copy_from_slice(&7u16.to_le_bytes())
        }));
        assert!(e.contains("names partition 2989, and the sequence"), "{e}");
    }

    #[test]
    fn a_block_of_another_size_is_refused() {
        let e = refused(with_both(|lvd, _| {
            lvd[212..216].copy_from_slice(&4096u32.to_le_bytes())
        }));
        assert!(e.contains("blocks of 4096 bytes"), "{e}");
    }

    #[test]
    fn a_partition_that_holds_no_file_set_of_udf_is_refused() {
        let e = refused(with_both(|_, pd| pd[25..31].copy_from_slice(b"+FDC01")));
        assert!(e.contains("holds \"+FDC01\""), "{e}");
    }

    #[test]
    fn a_file_set_in_a_partition_that_is_not_there_is_refused() {
        let e = refused(with_both(|lvd, _| {
            lvd[256..258].copy_from_slice(&1u16.to_le_bytes())
        }));
        assert!(e.contains("the file set is in partition 1"), "{e}");
    }
}
