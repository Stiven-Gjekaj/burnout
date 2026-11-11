//! A GUID partition table, which a drive of Windows To Go for UEFI needs.
//!
//! Windows finds its EFI system partition by the type of the partition. On
//! an MBR, the specialize pass of Windows 11 25H2 stopped with "Failed to
//! get system partition", also with type 0xEF, and on a GPT it went on,
//! measured on 2026-11-11.
//!
//! The table is a protective MBR, a header in sector 1 with 128 entries of
//! 128 bytes after it, and a copy of the entries and the header at the end
//! of the disk. The header and the entries each carry a CRC-32.

use burnout_core::crc32;

/// The type of an EFI system partition.
pub const EFI_SYSTEM: [u8; 16] = guid(
    0xC12A7328,
    0xF81F,
    0x11D2,
    [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
);
/// The type of a partition of Windows data.
pub const BASIC_DATA: [u8; 16] = guid(
    0xEBD0A0A2,
    0xB9E5,
    0x4433,
    [0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7],
);

const ENTRIES: usize = 128;
const ENTRY_BYTES: usize = 128;
const ENTRY_SECTORS: u64 = (ENTRIES * ENTRY_BYTES / 512) as u64;

/// A GUID in the byte order of a GPT, from its four fields.
pub const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

/// One partition of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GptPartition {
    pub kind: [u8; 16],
    pub id: [u8; 16],
    /// The first and the last sector, both in the partition.
    pub first: u64,
    pub last: u64,
    pub name: String,
}

/// The sectors of a GPT: the bytes for the start of the disk, and the bytes
/// for its last sectors, with the sector where they go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpt {
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
    pub tail_sector: u64,
}

/// The first and the last sector that a partition can take on a disk of
/// `sectors` sectors of 512 bytes.
pub fn usable(sectors: u64) -> (u64, u64) {
    (2 + ENTRY_SECTORS, sectors - 2 - ENTRY_SECTORS)
}

/// The table for a disk of `sectors` sectors of 512 bytes.
pub fn gpt(sectors: u64, disk: [u8; 16], partitions: &[GptPartition]) -> Result<Gpt, String> {
    let (first_usable, last_usable) = usable(sectors);
    if partitions.len() > ENTRIES {
        return Err(format!(
            "{} partitions, and a GPT holds {ENTRIES}",
            partitions.len()
        ));
    }
    let mut entries = vec![0u8; ENTRIES * ENTRY_BYTES];
    for (i, p) in partitions.iter().enumerate() {
        if p.first < first_usable || p.last > last_usable || p.first > p.last {
            return Err(format!(
                "partition {} goes from sector {} to {}, outside {first_usable} to {last_usable}",
                i + 1,
                p.first,
                p.last
            ));
        }
        let e = &mut entries[i * ENTRY_BYTES..(i + 1) * ENTRY_BYTES];
        e[0..16].copy_from_slice(&p.kind);
        e[16..32].copy_from_slice(&p.id);
        e[32..40].copy_from_slice(&p.first.to_le_bytes());
        e[40..48].copy_from_slice(&p.last.to_le_bytes());
        for (j, u) in p.name.encode_utf16().take(36).enumerate() {
            e[56 + 2 * j..58 + 2 * j].copy_from_slice(&u.to_le_bytes());
        }
    }
    let entries_crc = crc32(&entries);
    let header = |this: u64, other: u64, table: u64| {
        let mut h = vec![0u8; 512];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&this.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&disk);
        h[72..80].copy_from_slice(&table.to_le_bytes());
        h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    let mut mbr = vec![0u8; 512];
    let e = 446;
    mbr[e + 1..e + 4].copy_from_slice(&[0, 2, 0]);
    mbr[e + 4] = 0xEE;
    mbr[e + 5..e + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    mbr[e + 8..e + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[e + 12..e + 16].copy_from_slice(&((sectors - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    let mut head = mbr;
    head.extend_from_slice(&header(1, sectors - 1, 2));
    head.extend_from_slice(&entries);
    let mut tail = entries;
    tail.extend_from_slice(&header(sectors - 1, 1, sectors - 1 - ENTRY_SECTORS));
    Ok(Gpt {
        head,
        tail,
        tail_sector: sectors - 1 - ENTRY_SECTORS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_core::{read_table, PartitionType, TableKind};

    #[test]
    fn the_reader_of_burnout_takes_the_table_and_its_two_partitions() {
        let sectors = 128 * 2048;
        let parts = vec![
            GptPartition {
                kind: EFI_SYSTEM,
                id: [1; 16],
                first: 2048,
                last: 2048 * 33 - 1,
                name: "EFI system partition".to_string(),
            },
            GptPartition {
                kind: BASIC_DATA,
                id: [2; 16],
                first: 2048 * 64,
                last: usable(sectors).1,
                name: "Windows".to_string(),
            },
        ];
        let g = gpt(sectors, [9; 16], &parts).unwrap();
        assert_eq!(g.head.len(), 34 * 512);
        assert_eq!(g.tail.len(), 33 * 512);
        assert_eq!(g.tail_sector + 33, sectors);
        let mut disk = vec![0u8; (sectors * 512) as usize];
        disk[..g.head.len()].copy_from_slice(&g.head);
        let at = (g.tail_sector * 512) as usize;
        disk[at..at + g.tail.len()].copy_from_slice(&g.tail);
        let table = read_table(&disk[..1024 * 1024], 512, sectors * 512)
            .unwrap()
            .unwrap();
        assert_eq!(table.kind, TableKind::Gpt);
        assert_eq!(table.partitions.len(), 2);
        assert_eq!(table.partitions[0].first_byte, 2048 * 512);
        assert_eq!(table.partitions[0].bytes, 32 * 1024 * 1024);
        let PartitionType::Gpt { name, .. } = &table.partitions[1].kind else {
            panic!("a GPT partition")
        };
        assert_eq!(name, "Windows");
        assert!(table.partitions[1]
            .kind
            .describe()
            .starts_with("Microsoft basic data"));
        // The copy of the header names the copy of the entries.
        let backup = &disk[((sectors - 1) * 512) as usize..];
        assert_eq!(&backup[0..8], b"EFI PART");
        assert_eq!(
            u64::from_le_bytes(backup[72..80].try_into().unwrap()),
            g.tail_sector
        );
    }

    #[test]
    fn a_partition_outside_the_usable_sectors_is_refused() {
        let p = GptPartition {
            kind: BASIC_DATA,
            id: [2; 16],
            first: 10,
            last: 100,
            name: String::new(),
        };
        assert!(gpt(4096, [0; 16], &[p])
            .unwrap_err()
            .contains("outside 34 to 4062"));
    }

    #[test]
    fn the_guids_of_the_types_are_the_ones_of_uefi() {
        assert_eq!(&EFI_SYSTEM[..4], &[0x28, 0x73, 0x2A, 0xC1]);
        assert_eq!(
            &BASIC_DATA[8..],
            &[0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7]
        );
    }
}
