//! The directory entries of exFAT, and the rules for a name.
//!
//! Each entry is 32 bytes. The root directory holds one entry for the
//! allocation bitmap, one for the up-case table and one for the label. Each
//! file and each directory is a set of entries: a file entry, a stream
//! extension that says where the data is, and a name entry for each 15 units
//! of the name.

use super::sums::set_checksum;

/// The bytes of one directory entry.
pub(crate) const ENTRY_BYTES: usize = 32;

/// The type of each entry that this writes, with the bit that says it is in
/// use.
pub(crate) const BITMAP: u8 = 0x81;
pub(crate) const UPCASE: u8 = 0x82;
pub(crate) const LABEL: u8 = 0x83;
pub(crate) const FILE: u8 = 0x85;
pub(crate) const STREAM: u8 = 0xC0;
pub(crate) const NAME: u8 = 0xC1;

/// The attribute of a directory, and the one that Windows gives a new file.
pub(crate) const DIRECTORY: u16 = 0x10;
pub(crate) const ARCHIVE: u16 = 0x20;

/// The flags of a stream: the entry can have clusters, and its clusters are
/// one run that the FAT does not record.
pub(crate) const ALLOCATION_POSSIBLE: u8 = 0x01;
pub(crate) const NO_FAT_CHAIN: u8 = 0x02;

/// The units of a name that one name entry holds.
pub(crate) const UNITS_PER_NAME_ENTRY: usize = 15;

/// The longest name, in UTF-16 units.
pub(crate) const MAX_NAME_UNITS: usize = 255;

/// The longest label, in UTF-16 units.
pub(crate) const MAX_LABEL_UNITS: usize = 11;

/// 1980-01-01 at midnight, the first second that the timestamp of exFAT
/// holds. Every entry carries it, as on the FAT32 partition.
pub(crate) const FAT_EPOCH: u32 = (1 << 21) | (1 << 16);

/// A directory or a file, as its entry set records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
}

/// The entry of the allocation bitmap.
pub(crate) fn bitmap_entry(first_cluster: u32, bytes: u64) -> [u8; ENTRY_BYTES] {
    let mut e = [0u8; ENTRY_BYTES];
    e[0] = BITMAP;
    // The flags at byte 1 stay zero: this is the first bitmap, of one.
    e[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    e[24..32].copy_from_slice(&bytes.to_le_bytes());
    e
}

/// The entry of the up-case table.
pub(crate) fn upcase_entry(checksum: u32, first_cluster: u32, bytes: u64) -> [u8; ENTRY_BYTES] {
    let mut e = [0u8; ENTRY_BYTES];
    e[0] = UPCASE;
    e[4..8].copy_from_slice(&checksum.to_le_bytes());
    e[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    e[24..32].copy_from_slice(&bytes.to_le_bytes());
    e
}

/// The entry of the label, or `None` for a volume with no label.
///
/// exFAT keeps 11 UTF-16 units of the label, in the case it has. A character
/// that a name cannot hold becomes `_`.
pub(crate) fn label_entry(label: &str) -> Option<[u8; ENTRY_BYTES]> {
    let units = label_units(label);
    if units.is_empty() {
        return None;
    }
    let mut e = [0u8; ENTRY_BYTES];
    e[0] = LABEL;
    e[1] = units.len() as u8;
    for (at, unit) in units.iter().enumerate() {
        e[2 + 2 * at..4 + 2 * at].copy_from_slice(&unit.to_le_bytes());
    }
    Some(e)
}

/// The units of the label that go onto the volume.
fn label_units(label: &str) -> Vec<u16> {
    let mut units = Vec::new();
    for c in label.trim().chars() {
        let c = if fits_a_name(c) { c } else { '_' };
        if units.len() + c.len_utf16() > MAX_LABEL_UNITS {
            break;
        }
        let mut pair = [0u16; 2];
        units.extend_from_slice(c.encode_utf16(&mut pair));
    }
    units
}

/// How many entries a file or a directory with this name takes.
pub(crate) fn set_entries(name_units: usize) -> usize {
    2 + name_units.div_ceil(UNITS_PER_NAME_ENTRY)
}

/// The entry set of a file or a directory.
///
/// `name` is the name as it goes onto the volume, and `hash` the hash of it
/// in upper case. The data is one run of clusters from `first_cluster`, and
/// `data_length` says how many bytes of it the entry holds. A file of no
/// bytes has no cluster, and its first cluster is 0.
pub(crate) fn file_set(
    name: &[u16],
    hash: u16,
    kind: Kind,
    first_cluster: u32,
    data_length: u64,
) -> Vec<u8> {
    let names = name.len().div_ceil(UNITS_PER_NAME_ENTRY);
    let mut set = vec![0u8; (2 + names) * ENTRY_BYTES];

    let file = &mut set[..ENTRY_BYTES];
    file[0] = FILE;
    file[1] = (1 + names) as u8;
    let attributes = match kind {
        Kind::Dir => DIRECTORY,
        Kind::File => ARCHIVE,
    };
    file[4..6].copy_from_slice(&attributes.to_le_bytes());
    for at in [8, 12, 16] {
        file[at..at + 4].copy_from_slice(&FAT_EPOCH.to_le_bytes());
    }
    // The hundredths of each time stay zero. So does each offset from UTC,
    // which says that the time is local time, as FAT32 keeps it.

    let stream = &mut set[ENTRY_BYTES..2 * ENTRY_BYTES];
    stream[0] = STREAM;
    stream[1] = if first_cluster == 0 {
        ALLOCATION_POSSIBLE
    } else {
        ALLOCATION_POSSIBLE | NO_FAT_CHAIN
    };
    stream[3] = name.len() as u8;
    stream[4..6].copy_from_slice(&hash.to_le_bytes());
    // Every byte of the data is written, so the valid length is the length.
    stream[8..16].copy_from_slice(&data_length.to_le_bytes());
    stream[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    stream[24..32].copy_from_slice(&data_length.to_le_bytes());

    for (number, part) in name.chunks(UNITS_PER_NAME_ENTRY).enumerate() {
        let start = (2 + number) * ENTRY_BYTES;
        let entry = &mut set[start..start + ENTRY_BYTES];
        entry[0] = NAME;
        for (at, unit) in part.iter().enumerate() {
            entry[2 + 2 * at..4 + 2 * at].copy_from_slice(&unit.to_le_bytes());
        }
    }

    let sum = set_checksum(&set);
    set[2..4].copy_from_slice(&sum.to_le_bytes());
    set
}

/// The name as exFAT holds it, in UTF-16 units, or what is wrong with it.
pub(crate) fn name_units(name: &str) -> Result<Vec<u16>, String> {
    if let Some(bad) = name.chars().find(|c| !fits_a_name(*c)) {
        return Err(format!(
            "exFAT does not hold the character {bad:?} in a name"
        ));
    }
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.is_empty() || units.len() > MAX_NAME_UNITS {
        return Err(format!(
            "exFAT holds a name of 1 to {MAX_NAME_UNITS} UTF-16 units, and this one has {}",
            units.len()
        ));
    }
    Ok(units)
}

/// Whether a name of exFAT can hold this character.
fn fits_a_name(c: char) -> bool {
    !(c < ' ' || matches!(c, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    // Entries that macOS 26 wrote onto a volume of its own, read back from
    // the volume. The file entries carry the time of the day that macOS
    // wrote them, so a test compares everything else.

    const MACOS_LABEL: &str = "830756004500430054004f005200530000000000000000000000000000000000";
    const MACOS_BITMAP: &str = "810000000000000000000000000000000000000002000000fc07000000000000";
    const MACOS_UPCASE: &str = "820000000dd319e600000000000000000000000003000000cc16000000000000";

    const MACOS_HELLO: &str = "\
        85023b5c200000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
        c003000946300000050000000000000000000000070000000500000000000000\
        c100480065006c006c006f002e00740078007400000000000000000000000000";
    const MACOS_EMPTY: &str = "\
        850401a0200000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
        c001002cc1a50000000000000000000000000000000000000000000000000000\
        c100410020006e0061006d006500200074006800610074002000690073002000\
        c1006c006f006e0067006500720020007400680061006e002000660069006600\
        c1007400650065006e00200075006e006900740073002e007400780074000000";
    const MACOS_SUB_DIR: &str = "\
        8502a857300000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
        c00300076cae00000010000000000000000000000b0000000010000000000000\
        c100530075006200200044006900720000000000000000000000000000000000";

    #[test]
    fn the_entries_of_the_root_are_the_ones_that_macos_wrote() {
        assert_eq!(label_entry("VECTORS").unwrap().to_vec(), hex(MACOS_LABEL));
        assert_eq!(bitmap_entry(2, 0x7FC).to_vec(), hex(MACOS_BITMAP));
        assert_eq!(
            upcase_entry(0xE619_D30D, 3, 5836).to_vec(),
            hex(MACOS_UPCASE)
        );
    }

    /// Compare a set with one that macOS wrote: every byte but the times,
    /// and the checksum, which covers the times.
    fn same_but_the_times(ours: &[u8], macos: &str) {
        let macos = hex(macos);
        assert_eq!(ours.len(), macos.len());
        assert_eq!(ours[..2], macos[..2], "the type and the count");
        assert_eq!(
            ours[ENTRY_BYTES..],
            macos[ENTRY_BYTES..],
            "the stream and the name"
        );
    }

    #[test]
    fn a_file_set_is_the_one_that_macos_wrote() {
        let set = file_set(&units("Hello.txt"), 0x3046, Kind::File, 7, 5);
        same_but_the_times(&set, MACOS_HELLO);
        assert_eq!(set[4..6], hex(MACOS_HELLO)[4..6], "the attributes");
    }

    #[test]
    fn a_file_of_no_bytes_has_no_cluster_and_no_run() {
        let name = units("A name that is longer than fifteen units.txt");
        let set = file_set(&name, 0xA5C1, Kind::File, 0, 0);
        same_but_the_times(&set, MACOS_EMPTY);
        assert_eq!(set[ENTRY_BYTES + 1], ALLOCATION_POSSIBLE);
    }

    #[test]
    fn a_directory_set_is_the_one_that_macos_wrote() {
        // macOS also sets the archive bit on a directory, and Windows does
        // not. This follows Windows.
        let set = file_set(&units("Sub Dir"), 0xAE6C, Kind::Dir, 11, 4096);
        same_but_the_times(&set, MACOS_SUB_DIR);
        assert_eq!(u16::from_le_bytes([set[4], set[5]]), DIRECTORY);
    }

    #[test]
    fn every_time_is_the_fat_epoch_in_local_time() {
        let set = file_set(&units("a"), 0, Kind::File, 0, 0);
        for at in [8, 12, 16] {
            assert_eq!(set[at..at + 4], [0x00, 0x00, 0x21, 0x00]);
        }
        assert_eq!(set[20..25], [0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_set_carries_its_own_checksum() {
        let set = file_set(&units("setup.exe"), 0x1234, Kind::File, 40, 3000);
        let stored = u16::from_le_bytes([set[2], set[3]]);
        assert_eq!(set_checksum(&set), stored);
    }

    #[test]
    fn a_large_file_keeps_its_length_in_64_bits() {
        let bytes = 6 * (1u64 << 30) + 12_345;
        let set = file_set(&units("install.wim"), 0, Kind::File, 100, bytes);
        let stream = &set[ENTRY_BYTES..2 * ENTRY_BYTES];
        assert_eq!(u64::from_le_bytes(stream[8..16].try_into().unwrap()), bytes);
        assert_eq!(
            u64::from_le_bytes(stream[24..32].try_into().unwrap()),
            bytes
        );
    }

    #[test]
    fn a_name_takes_one_name_entry_for_each_fifteen_units() {
        assert_eq!(set_entries(1), 3);
        assert_eq!(set_entries(15), 3);
        assert_eq!(set_entries(16), 4);
        assert_eq!(set_entries(255), 19);
        let set = file_set(&vec![0x41; 255], 0, Kind::File, 0, 0);
        assert_eq!(set.len(), 19 * ENTRY_BYTES);
        assert_eq!(set[1], 18);
    }

    #[test]
    fn a_name_that_exfat_can_hold_goes_through() {
        for name in [
            "setup.exe",
            "A long name with spaces.txt",
            "\u{dc}berpr\u{fc}fung.txt",
        ] {
            assert_eq!(name_units(name).unwrap(), units(name));
        }
        assert!(name_units(&"a".repeat(255)).is_ok());
    }

    #[test]
    fn a_name_with_a_character_that_exfat_refuses_is_refused() {
        for name in [
            "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a\\b", "a|b", "a\u{1}b",
        ] {
            let e = name_units(name).unwrap_err();
            assert!(e.contains("does not hold the character"), "{name}: {e}");
        }
    }

    #[test]
    fn a_name_longer_than_255_units_is_refused() {
        let e = name_units(&"a".repeat(256)).unwrap_err();
        assert!(e.contains("this one has 256"), "{e}");
        // A character past the first plane takes two units.
        let e = name_units(&"\u{1F600}".repeat(128)).unwrap_err();
        assert!(e.contains("this one has 256"), "{e}");
    }

    #[test]
    fn a_label_keeps_eleven_units_and_its_case() {
        assert_eq!(label_units("Install"), units("Install"));
        assert_eq!(label_units("  Windows 11 x64  "), units("Windows 11 "));
        assert_eq!(label_units("a:b"), units("a_b"));
        assert_eq!(label_units("\u{dc}ber"), units("\u{dc}ber"));
        // A pair of units does not split at the limit.
        assert_eq!(label_units("abcdefghij\u{1F600}"), units("abcdefghij"));
        assert_eq!(label_entry("   "), None);
    }
}
