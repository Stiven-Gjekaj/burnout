//! The partition table of a drive, read from the drive itself.
//!
//! A write to one partition needs to know where the partition starts and how
//! long it is. Each host names partitions in its own way, but each one counts
//! them the same way: an entry of an MBR by its place from one to four, and
//! an entry of a GPT by its place from one. So Burnout reads the table from
//! the first bytes of the drive, with the same code on every host, and its
//! numbers are the numbers that the host shows: `sdb2`, `disk4s2` and
//! partition 2 of Disk Management are all partition 2 here.
//!
//! An MBR that marks its first entry 0xEE protects a GPT, and the GPT is the
//! table then. Its header and its entries carry a CRC-32 each, and a table
//! whose CRC does not match is refused rather than guessed at.

use std::fmt;

use crate::{Error, Result};

/// How many bytes of the start of a drive [`read_table`] needs.
///
/// The GPT of every tool puts 128 entries of 128 bytes after its header, and
/// they end in the first 34 sectors of 512 bytes or the first 6 sectors of
/// 4096 bytes. One mebibyte holds that with room to spare.
pub const TABLE_BYTES: usize = 1024 * 1024;

/// The MBR type of an entry that protects a GPT.
const PROTECTIVE: u8 = 0xEE;

/// The MBR types of an extended partition, which holds more partitions in a
/// chain of its own.
const EXTENDED: [u8; 3] = [0x05, 0x0F, 0x85];

/// Which kind of table the drive holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableKind {
    Mbr,
    Gpt,
}

impl fmt::Display for TableKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TableKind::Mbr => "MBR",
            TableKind::Gpt => "GPT",
        })
    }
}

/// What the table says a partition holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartitionType {
    /// The type byte of an MBR entry.
    Mbr(u8),
    /// The type of a GPT entry, and the name that the entry carries.
    Gpt { type_guid: [u8; 16], name: String },
}

impl PartitionType {
    /// An extended partition of an MBR holds other partitions, and a write to
    /// it would erase all of them.
    pub fn is_extended(&self) -> bool {
        matches!(self, PartitionType::Mbr(t) if EXTENDED.contains(t))
    }

    /// The type as a person reads it.
    pub fn describe(&self) -> String {
        match self {
            PartitionType::Mbr(t) => match mbr_type_name(*t) {
                Some(name) => format!("{name} (0x{t:02X})"),
                None => format!("type 0x{t:02X}"),
            },
            PartitionType::Gpt { type_guid, name } => {
                let kind = gpt_type_name(type_guid)
                    .map(str::to_string)
                    .unwrap_or_else(|| guid_text(type_guid));
                if name.is_empty() {
                    kind
                } else {
                    format!("{kind}, named \"{name}\"")
                }
            }
        }
    }
}

/// One partition of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    /// The number that the host shows, counted from one.
    pub number: u32,
    /// The first byte of the partition on the drive.
    pub first_byte: u64,
    /// The length of the partition in bytes.
    pub bytes: u64,
    pub kind: PartitionType,
}

/// The table of a drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionTable {
    pub kind: TableKind,
    /// Every entry that is in use, in the order of its number.
    pub partitions: Vec<Partition>,
}

impl PartitionTable {
    /// The partition with this number.
    pub fn partition(&self, number: u32) -> Option<&Partition> {
        self.partitions.iter().find(|p| p.number == number)
    }
}

/// Read the table from `head`, the first bytes of a drive of `drive_bytes`
/// with sectors of `sector_size` bytes.
///
/// A drive with no table gives `None`. A table that contradicts itself, two
/// partitions that overlap, or a partition that reaches past the end of the
/// drive, is an error, because a
/// write that trusted it could land outside the partition.
pub fn read_table(
    head: &[u8],
    sector_size: u32,
    drive_bytes: u64,
) -> Result<Option<PartitionTable>> {
    if head.len() < 512 || head[510] != 0x55 || head[511] != 0xAA {
        return Ok(None);
    }
    let entries: Vec<&[u8]> = (0..4).map(|i| &head[446 + 16 * i..462 + 16 * i]).collect();
    let in_use: Vec<(u32, &[u8])> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e[4] != 0)
        .map(|(i, e)| (i as u32 + 1, *e))
        .collect();
    if in_use.is_empty() {
        return Ok(None);
    }
    if in_use.iter().any(|(_, e)| e[4] == PROTECTIVE) {
        return read_gpt(head, sector_size, drive_bytes).map(Some);
    }

    let sector = sector_size as u64;
    let mut partitions = Vec::new();
    for (number, e) in in_use {
        let first = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64;
        let count = u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64;
        let partition = Partition {
            number,
            first_byte: first * sector,
            bytes: count * sector,
            kind: PartitionType::Mbr(e[4]),
        };
        check_inside(&partition, drive_bytes)?;
        partitions.push(partition);
    }
    check_apart(&partitions)?;
    Ok(Some(PartitionTable {
        kind: TableKind::Mbr,
        partitions,
    }))
}

fn read_gpt(head: &[u8], sector_size: u32, drive_bytes: u64) -> Result<PartitionTable> {
    let sector = sector_size as usize;
    let header = head
        .get(sector..sector + 92)
        .ok_or_else(|| damaged("the drive ends before the header of its GPT"))?;
    if &header[..8] != b"EFI PART" {
        return Err(damaged(
            "the MBR protects a GPT, and sector 1 holds no GPT header",
        ));
    }
    let header_size = u32_at(header, 12) as usize;
    if !(92..=sector).contains(&header_size) {
        return Err(damaged("the GPT header gives a size that no header has"));
    }
    let mut copy = head[sector..sector + header_size].to_vec();
    copy[16..20].fill(0);
    if crc32(&copy) != u32_at(header, 16) {
        return Err(damaged("the CRC of the GPT header does not match it"));
    }

    let entries_lba = u64_at(header, 72);
    let count = u32_at(header, 80) as usize;
    let entry_size = u32_at(header, 84) as usize;
    if entry_size < 128 || entry_size % 8 != 0 || count > 4096 {
        return Err(damaged(
            "the GPT header gives a size or a count of entries that no GPT has",
        ));
    }
    let start = entries_lba as usize * sector;
    let table = head.get(start..start + count * entry_size).ok_or_else(|| {
        damaged("the entries of the GPT are not in the first mebibyte of the drive")
    })?;
    if crc32(table) != u32_at(header, 88) {
        return Err(damaged("the CRC of the GPT entries does not match them"));
    }

    let mut partitions = Vec::new();
    for (i, e) in table.chunks_exact(entry_size).enumerate() {
        let type_guid: [u8; 16] = e[..16].try_into().unwrap();
        if type_guid == [0; 16] {
            continue;
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        if last < first {
            return Err(damaged("an entry of the GPT ends before it starts"));
        }
        let partition = Partition {
            number: i as u32 + 1,
            first_byte: first * sector as u64,
            bytes: (last - first + 1) * sector as u64,
            kind: PartitionType::Gpt {
                type_guid,
                name: utf16_name(&e[56..128]),
            },
        };
        check_inside(&partition, drive_bytes)?;
        partitions.push(partition);
    }
    check_apart(&partitions)?;
    Ok(PartitionTable {
        kind: TableKind::Gpt,
        partitions,
    })
}

/// The partition `number` of `table`, checked for a write of `image_bytes`.
///
/// `drive` names the drive in a message. A drive with no table, a number that
/// the table does not hold, an extended partition, and a partition smaller
/// than the image are each refused.
pub fn choose_partition(
    table: Option<&PartitionTable>,
    number: u32,
    drive: &str,
    image_bytes: u64,
) -> Result<Partition> {
    let table = table.ok_or_else(|| Error::NoPartitionTable {
        drive: drive.to_string(),
    })?;
    let partition = table
        .partition(number)
        .ok_or_else(|| Error::NoSuchPartition {
            drive: drive.to_string(),
            number,
            numbers: numbers_text(table),
        })?;
    if partition.kind.is_extended() {
        return Err(Error::ExtendedPartition { number });
    }
    if image_bytes > partition.bytes {
        return Err(Error::PartitionTooSmallForImage {
            number,
            image_bytes,
            partition_bytes: partition.bytes,
        });
    }
    Ok(partition.clone())
}

/// The numbers of a table as a person reads them: "1, 2 and 4".
fn numbers_text(table: &PartitionTable) -> String {
    let n: Vec<String> = table
        .partitions
        .iter()
        .map(|p| p.number.to_string())
        .collect();
    match n.len() {
        0 => "none".to_string(),
        1 => n[0].clone(),
        _ => format!("{} and {}", n[..n.len() - 1].join(", "), n[n.len() - 1]),
    }
}

fn check_inside(p: &Partition, drive_bytes: u64) -> Result<()> {
    if p.bytes == 0 || p.first_byte.saturating_add(p.bytes) > drive_bytes {
        return Err(damaged(&format!(
            "partition {} reaches past the end of the drive",
            p.number
        )));
    }
    Ok(())
}

/// Refuse two partitions that share a byte. A write into one of them would
/// also write into the other.
fn check_apart(partitions: &[Partition]) -> Result<()> {
    for (i, a) in partitions.iter().enumerate() {
        for b in &partitions[i + 1..] {
            if a.first_byte < b.first_byte + b.bytes && b.first_byte < a.first_byte + a.bytes {
                return Err(damaged(&format!(
                    "partitions {} and {} overlap",
                    a.number, b.number
                )));
            }
        }
    }
    Ok(())
}

fn damaged(detail: &str) -> Error {
    Error::TableDamaged {
        detail: detail.to_string(),
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The name of a GPT entry: UTF-16 in 72 bytes, ended by a zero.
fn utf16_name(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A GUID as it is written: the first three fields in little-endian order.
fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        u32::from_le_bytes(g[0..4].try_into().unwrap()),
        u16::from_le_bytes(g[4..6].try_into().unwrap()),
        u16::from_le_bytes(g[6..8].try_into().unwrap()),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

fn gpt_type_name(g: &[u8; 16]) -> Option<&'static str> {
    Some(match guid_text(g).as_str() {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI system partition",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Microsoft basic data",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recovery",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux file system",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot partition",
        _ => return None,
    })
}

fn mbr_type_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0x01 | 0x04 | 0x06 | 0x0E => "FAT",
        0x0B | 0x0C => "FAT32",
        0x07 => "NTFS or exFAT",
        0x05 | 0x0F | 0x85 => "extended",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0xEF => "EFI system partition",
        _ => return None,
    })
}

/// The CRC-32 of IEEE 802.3, which a GPT uses.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn mbr(entries: &[(u8, u32, u32)]) -> Vec<u8> {
        let mut head = vec![0u8; TABLE_BYTES];
        for (i, (kind, first, count)) in entries.iter().enumerate() {
            let e = 446 + 16 * i;
            head[e + 4] = *kind;
            head[e + 8..e + 12].copy_from_slice(&first.to_le_bytes());
            head[e + 12..e + 16].copy_from_slice(&count.to_le_bytes());
        }
        head[510] = 0x55;
        head[511] = 0xAA;
        head
    }

    /// A GPT with these entries: type, first LBA, last LBA, name.
    fn gpt(sector: usize, entries: &[([u8; 16], u64, u64, &str)]) -> Vec<u8> {
        let mut head = mbr(&[(PROTECTIVE, 1, u32::MAX)]);
        let table_at = 2 * sector;
        for (i, (kind, first, last, name)) in entries.iter().enumerate() {
            let e = table_at + 128 * i;
            head[e..e + 16].copy_from_slice(kind);
            head[e + 32..e + 40].copy_from_slice(&first.to_le_bytes());
            head[e + 40..e + 48].copy_from_slice(&last.to_le_bytes());
            for (j, u) in name.encode_utf16().enumerate() {
                head[e + 56 + 2 * j..e + 58 + 2 * j].copy_from_slice(&u.to_le_bytes());
            }
        }
        let h = sector;
        head[h..h + 8].copy_from_slice(b"EFI PART");
        head[h + 12..h + 16].copy_from_slice(&92u32.to_le_bytes());
        head[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes());
        head[h + 80..h + 84].copy_from_slice(&128u32.to_le_bytes());
        head[h + 84..h + 88].copy_from_slice(&128u32.to_le_bytes());
        let crc = crc32(&head[table_at..table_at + 128 * 128]);
        head[h + 88..h + 92].copy_from_slice(&crc.to_le_bytes());
        let crc = crc32(&head[h..h + 92]);
        head[h + 16..h + 20].copy_from_slice(&crc.to_le_bytes());
        head
    }

    const BASIC_DATA: [u8; 16] = [
        0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99,
        0xC7,
    ];
    const LINUX: [u8; 16] = [
        0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D,
        0xE4,
    ];

    #[test]
    fn the_crc_is_the_one_of_ieee_802_3() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn an_mbr_counts_its_entries_from_one_and_skips_the_empty_ones() {
        let head = mbr(&[(0x0C, 2048, 2048), (0, 0, 0), (0x83, 4096, 8192)]);
        let t = read_table(&head, 512, 64 * MIB).unwrap().unwrap();
        assert_eq!(t.kind, TableKind::Mbr);
        assert_eq!(t.partitions.len(), 2);
        let p = t.partition(3).unwrap();
        assert_eq!(p.first_byte, 4096 * 512);
        assert_eq!(p.bytes, 8192 * 512);
        assert_eq!(p.kind.describe(), "Linux (0x83)");
        assert!(t.partition(2).is_none());
    }

    #[test]
    fn a_drive_of_4096_byte_sectors_counts_its_own_sectors() {
        let head = mbr(&[(0x0C, 256, 256)]);
        let t = read_table(&head, 4096, 64 * MIB).unwrap().unwrap();
        assert_eq!(t.partitions[0].first_byte, MIB);
        assert_eq!(t.partitions[0].bytes, MIB);
    }

    #[test]
    fn a_drive_with_no_table_has_no_partitions() {
        assert!(read_table(&vec![0u8; 512], 512, MIB).unwrap().is_none());
        assert!(read_table(&mbr(&[]), 512, MIB).unwrap().is_none());
    }

    #[test]
    fn an_extended_partition_says_so() {
        let head = mbr(&[(0x0F, 2048, 2048)]);
        let t = read_table(&head, 512, 64 * MIB).unwrap().unwrap();
        assert!(t.partitions[0].kind.is_extended());
        assert!(!PartitionType::Mbr(0x0C).is_extended());
    }

    #[test]
    fn a_partition_past_the_end_of_the_drive_is_refused() {
        let head = mbr(&[(0x0C, 2048, 1_000_000)]);
        let e = read_table(&head, 512, 64 * MIB).unwrap_err();
        assert_eq!(e.kind(), "table_damaged");
        assert!(
            e.to_string().contains("partition 1 reaches past the end"),
            "{e}"
        );
    }

    #[test]
    fn two_partitions_that_overlap_are_refused() {
        let head = mbr(&[(0x0C, 2048, 4096), (0x83, 6143, 2048)]);
        let e = read_table(&head, 512, 64 * MIB).unwrap_err();
        assert_eq!(e.kind(), "table_damaged");
        assert!(e.to_string().contains("partitions 1 and 2 overlap"), "{e}");
        // The partition after the last byte of the first one is apart.
        let head = mbr(&[(0x0C, 2048, 4096), (0x83, 6144, 2048)]);
        assert_eq!(
            read_table(&head, 512, 64 * MIB)
                .unwrap()
                .unwrap()
                .partitions
                .len(),
            2
        );
    }

    #[test]
    fn a_gpt_gives_its_entries_with_their_numbers_types_and_names() {
        for sector in [512usize, 4096] {
            let head = gpt(
                sector,
                &[
                    (BASIC_DATA, 34, 2081, "DATA"),
                    ([0; 16], 0, 0, ""),
                    (LINUX, 4096, 8191, ""),
                ],
            );
            let t = read_table(&head, sector as u32, 4096 * MIB)
                .unwrap()
                .unwrap();
            assert_eq!(t.kind, TableKind::Gpt);
            assert_eq!(t.partitions.len(), 2);
            let p1 = t.partition(1).unwrap();
            assert_eq!(p1.first_byte, 34 * sector as u64);
            assert_eq!(p1.bytes, 2048 * sector as u64);
            assert_eq!(p1.kind.describe(), "Microsoft basic data, named \"DATA\"");
            let p3 = t.partition(3).unwrap();
            assert_eq!(p3.kind.describe(), "Linux file system");
        }
    }

    #[test]
    fn a_gpt_with_a_wrong_crc_is_refused() {
        let mut head = gpt(512, &[(BASIC_DATA, 34, 2081, "")]);
        head[2 * 512 + 40] ^= 1; // the last LBA of entry 1
        let e = read_table(&head, 512, 64 * MIB).unwrap_err();
        assert!(e.to_string().contains("CRC of the GPT entries"), "{e}");

        let mut head = gpt(512, &[(BASIC_DATA, 34, 2081, "")]);
        head[512 + 72] ^= 1;
        let e = read_table(&head, 512, 64 * MIB).unwrap_err();
        assert!(e.to_string().contains("CRC of the GPT header"), "{e}");
    }

    #[test]
    fn a_protective_mbr_with_no_gpt_behind_it_is_refused() {
        let head = mbr(&[(PROTECTIVE, 1, 1000)]);
        let e = read_table(&head, 512, 64 * MIB).unwrap_err();
        assert!(e.to_string().contains("no GPT header"), "{e}");
    }

    #[test]
    fn a_partition_is_chosen_by_its_number_and_checked() {
        let head = mbr(&[(0x0C, 2048, 2048), (0x0F, 4096, 4096), (0x83, 8192, 2048)]);
        let t = read_table(&head, 512, 64 * MIB).unwrap();
        let p = choose_partition(t.as_ref(), 3, "SanDisk", MIB).unwrap();
        assert_eq!(p.first_byte, 8192 * 512);

        let e = choose_partition(t.as_ref(), 4, "SanDisk", 1).unwrap_err();
        assert_eq!(
            e.to_string(),
            "SanDisk holds no partition 4. Its partitions are 1, 2 and 3"
        );
        let e = choose_partition(t.as_ref(), 2, "SanDisk", 1).unwrap_err();
        assert_eq!(e.kind(), "extended_partition");
        let e = choose_partition(t.as_ref(), 1, "SanDisk", MIB + 1).unwrap_err();
        assert_eq!(e.kind(), "partition_too_small_for_image");
        let e = choose_partition(None, 1, "SanDisk", 1).unwrap_err();
        assert_eq!(e.kind(), "no_partition_table");
    }

    #[test]
    fn an_image_that_fills_the_partition_exactly_fits() {
        let head = mbr(&[(0x0C, 2048, 2048)]);
        let t = read_table(&head, 512, 64 * MIB).unwrap();
        assert!(choose_partition(t.as_ref(), 1, "d", MIB).is_ok());
    }

    #[test]
    fn an_unknown_type_gives_its_guid() {
        let mut kind = [0u8; 16];
        kind[0] = 0x78;
        kind[15] = 0x9A;
        let t = PartitionType::Gpt {
            type_guid: kind,
            name: String::new(),
        };
        assert_eq!(t.describe(), "00000078-0000-0000-0000-00000000009A");
    }
}
