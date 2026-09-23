//! The records of a directory of ISO 9660, and the names in them.

/// The flags of a record.
pub(crate) const DIRECTORY: u8 = 0x02;
pub(crate) const ASSOCIATED: u8 = 0x04;
pub(crate) const MULTI_EXTENT: u8 = 0x80;

/// The fixed part of a record, before its name.
const FIXED: usize = 33;

/// The sector that a record stays inside.
const SECTOR: usize = 2048;

/// One record of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// The first block of the data, after any extended attribute record.
    pub extent: u32,
    /// The bytes of the data.
    pub length: u32,
    pub flags: u8,
    /// The identifier as the record keeps it.
    pub id: Vec<u8>,
    /// The system use area, where Rock Ridge keeps its entries.
    pub system_use: Vec<u8>,
}

impl Record {
    pub(crate) fn is_dir(&self) -> bool {
        self.flags & DIRECTORY != 0
    }

    /// The record of the directory itself, which ISO 9660 names with a zero
    /// byte.
    pub(crate) fn is_self(&self) -> bool {
        self.id == [0]
    }

    /// The record of the directory above, named with a one byte.
    pub(crate) fn is_parent(&self) -> bool {
        self.id == [1]
    }
}

/// Every record in the bytes of a directory.
///
/// A record never crosses the end of a sector. A length of zero at the start
/// of a record means that the rest of the sector is empty.
pub(crate) fn records(bytes: &[u8]) -> Result<Vec<Record>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let length = bytes[at] as usize;
        if length == 0 {
            at = (at / SECTOR + 1) * SECTOR;
            continue;
        }
        let sector_end = ((at / SECTOR + 1) * SECTOR).min(bytes.len());
        if length < FIXED + 1 || at + length > sector_end {
            return Err(format!(
                "a record at byte {at} of the directory has a length of {length}, which does not fit its sector"
            ));
        }
        out.push(record(&bytes[at..at + length], at)?);
        at += length;
    }
    Ok(out)
}

fn record(r: &[u8], at: usize) -> Result<Record, String> {
    let extended = r[1] as u32;
    let u32_at = |i: usize| u32::from_le_bytes(r[i..i + 4].try_into().unwrap());
    if r[26] != 0 || r[27] != 0 {
        return Err(format!(
            "the record at byte {at} of the directory is of an interleaved file, which Burnout does not read"
        ));
    }
    let id_length = r[32] as usize;
    // A name of even length has one byte of padding after it.
    let system_use = FIXED + id_length + (1 - id_length % 2);
    if FIXED + id_length > r.len() || id_length == 0 {
        return Err(format!(
            "the record at byte {at} of the directory has a name that does not fit it"
        ));
    }
    Ok(Record {
        extent: u32_at(2) + extended,
        length: u32_at(10),
        flags: r[25],
        id: r[FIXED..FIXED + id_length].to_vec(),
        system_use: r.get(system_use..).unwrap_or(&[]).to_vec(),
    })
}

/// A plain name as a host shows it: without its version, and without a dot
/// at its end.
pub(crate) fn plain_name(id: &[u8]) -> Result<String, String> {
    // ISO 9660 keeps a name in ASCII. A byte past it is from a tool that
    // did not follow the rules, and it reads as the character of that value.
    let name: String = id.iter().map(|b| *b as char).collect();
    clean(name)
}

/// A Joliet name: UTF-16, the high byte first, without its version.
pub(crate) fn joliet_name(id: &[u8]) -> Result<String, String> {
    if id.len() % 2 != 0 {
        return Err("a Joliet name has an odd number of bytes".to_string());
    }
    let units: Vec<u16> = id
        .chunks_exact(2)
        .map(|p| u16::from_be_bytes([p[0], p[1]]))
        .collect();
    let name = String::from_utf16(&units).map_err(|_| "a Joliet name is not valid UTF-16")?;
    clean(name)
}

/// A name without `;1` and without a dot at its end.
fn clean(name: String) -> Result<String, String> {
    let base = match name.rfind(';') {
        Some(at) => &name[..at],
        None => &name,
    };
    let base = base.strip_suffix('.').unwrap_or(base);
    if base.is_empty() || base.contains('/') || base == "." || base == ".." {
        return Err(format!("the name {name:?} is not one that a tree can hold"));
    }
    Ok(base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::record_bytes;

    #[test]
    fn a_record_gives_its_extent_its_length_its_flags_and_its_name() {
        let bytes = record_bytes(40, 5000, DIRECTORY, b"BOOT", b"");
        let r = records(&bytes).unwrap().remove(0);
        assert_eq!((r.extent, r.length, r.flags), (40, 5000, DIRECTORY));
        assert_eq!(r.id, b"BOOT");
        assert!(r.is_dir());
        assert!(r.system_use.is_empty());
    }

    #[test]
    fn the_system_use_area_starts_after_the_padding_of_an_even_name() {
        let odd = record_bytes(1, 1, 0, b"ABC", b"SU");
        let even = record_bytes(1, 1, 0, b"AB", b"SU");
        assert_eq!(records(&odd).unwrap()[0].system_use, b"SU");
        assert_eq!(records(&even).unwrap()[0].system_use, b"SU");
    }

    #[test]
    fn an_extended_attribute_record_moves_the_start_of_the_data() {
        let mut bytes = record_bytes(40, 10, 0, b"A", b"");
        bytes[1] = 2;
        assert_eq!(records(&bytes).unwrap()[0].extent, 42);
    }

    #[test]
    fn a_zero_length_skips_to_the_next_sector() {
        let mut bytes = record_bytes(1, 1, 0, b"A", b"");
        bytes.resize(2048, 0);
        bytes.extend(record_bytes(2, 2, 0, b"B", b""));
        let ids: Vec<Vec<u8>> = records(&bytes).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [b"A".to_vec(), b"B".to_vec()]);
    }

    #[test]
    fn a_record_that_crosses_its_sector_is_refused() {
        // 59 records of 34 bytes end at byte 2006, and a record of 50 bytes
        // there runs 8 bytes into the next sector.
        let mut bytes = Vec::new();
        for _ in 0..59 {
            bytes.extend(record_bytes(1, 1, 0, b"A", b""));
        }
        assert_eq!(bytes.len(), 2006);
        bytes.extend(record_bytes(1, 1, 0, &[b'B'; 17], b""));
        bytes.resize(4096, 0);
        let e = records(&bytes).unwrap_err();
        assert!(e.contains("at byte 2006"), "{e}");
    }

    #[test]
    fn an_interleaved_file_is_refused() {
        let mut bytes = record_bytes(1, 1, 0, b"A", b"");
        bytes[26] = 1;
        assert!(records(&bytes).unwrap_err().contains("interleaved"));
    }

    #[test]
    fn a_plain_name_loses_its_version_and_its_last_dot() {
        assert_eq!(plain_name(b"SETUP.EXE;1").unwrap(), "SETUP.EXE");
        assert_eq!(plain_name(b"UBUNTU.;1").unwrap(), "UBUNTU");
        assert_eq!(plain_name(b"BOOT").unwrap(), "BOOT");
        assert!(plain_name(b";1").is_err());
        assert!(plain_name(b"A/B;1").is_err());
    }

    #[test]
    fn a_joliet_name_is_utf16_with_the_high_byte_first() {
        let id: Vec<u8> = "\u{dc}berpr\u{fc}fung.txt;1"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(joliet_name(&id).unwrap(), "\u{dc}berpr\u{fc}fung.txt");
        assert!(joliet_name(&[0x00]).is_err());
        assert!(joliet_name(&[0xD8, 0x00]).is_err(), "half of a pair");
    }
}
