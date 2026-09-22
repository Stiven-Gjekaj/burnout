//! The checksums of exFAT, as its specification gives them.
//!
//! All four are one step, again and again: turn the sum one bit to the
//! right, and add the next byte. Only the width of the sum and the bytes that
//! it skips differ.

/// The checksum of a boot region, over its first 11 sectors.
///
/// It skips the volume flags and the percent in use, at bytes 106, 107 and
/// 112, because a host changes those two fields and writes no new sum.
pub(crate) fn boot_checksum(sectors: &[u8]) -> u32 {
    sectors
        .iter()
        .enumerate()
        .filter(|(at, _)| !matches!(at, 106 | 107 | 112))
        .fold(0u32, |sum, (_, byte)| {
            sum.rotate_right(1).wrapping_add(*byte as u32)
        })
}

/// The checksum of a directory entry set, over each of its entries.
///
/// It skips bytes 2 and 3, which hold the checksum itself.
pub(crate) fn set_checksum(entries: &[u8]) -> u16 {
    entries
        .iter()
        .enumerate()
        .filter(|(at, _)| !matches!(at, 2 | 3))
        .fold(0u16, |sum, (_, byte)| {
            sum.rotate_right(1).wrapping_add(*byte as u16)
        })
}

/// The hash of a name, over its UTF-16 units in upper case, low byte first.
pub(crate) fn name_hash(upcased: &[u16]) -> u16 {
    upcased
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .fold(0u16, |sum, byte| {
            sum.rotate_right(1).wrapping_add(byte as u16)
        })
}

/// The checksum of an up-case table, over every byte of it.
pub(crate) fn table_checksum(table: &[u8]) -> u32 {
    table.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(*byte as u32)
    })
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

    /// Entry sets that the exFAT driver of macOS 26 wrote, read back from
    /// the volume. Each comes with the name, in upper case, that its hash is
    /// of. They test this code against another implementation, and not
    /// against itself.
    const MACOS_SETS: [(&str, &str); 4] = [
        (
            "HELLO.TXT",
            "85023b5c200000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
             c003000946300000050000000000000000000000070000000500000000000000\
             c100480065006c006c006f002e00740078007400000000000000000000000000",
        ),
        (
            "\u{dc}BERPR\u{dc}FUNG.TXT",
            "8502f953200000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
             c003000fc7c20000010000000000000000000000090000000100000000000000\
             c100dc0062006500720070007200fc00660075006e0067002e00740078007400",
        ),
        (
            "A NAME THAT IS LONGER THAN FIFTEEN UNITS.TXT",
            "850401a0200000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
             c001002cc1a50000000000000000000000000000000000000000000000000000\
             c100410020006e0061006d006500200074006800610074002000690073002000\
             c1006c006f006e0067006500720020007400680061006e002000660069006600\
             c1007400650065006e00200075006e006900740073002e007400780074000000",
        ),
        (
            "SUB DIR",
            "8502a857300000002fab365d2fab365d2fab365d8a8af8f8f800000000000000\
             c00300076cae00000010000000000000000000000b0000000010000000000000\
             c100530075006200200044006900720000000000000000000000000000000000",
        ),
    ];

    #[test]
    fn the_checksum_of_each_set_is_the_one_that_macos_wrote() {
        for (name, set) in MACOS_SETS {
            let set = hex(set);
            let stored = u16::from_le_bytes([set[2], set[3]]);
            assert_eq!(set_checksum(&set), stored, "{name}");
        }
    }

    #[test]
    fn the_hash_of_each_name_is_the_one_that_macos_wrote() {
        for (name, set) in MACOS_SETS {
            let set = hex(set);
            // The hash sits in the stream extension, the second entry.
            let stored = u16::from_le_bytes([set[36], set[37]]);
            assert_eq!(name_hash(&units(name)), stored, "{name}");
        }
    }

    #[test]
    fn the_checksum_of_a_set_does_not_read_its_own_two_bytes() {
        let mut set = hex(MACOS_SETS[0].1);
        let before = set_checksum(&set);
        set[2] ^= 0xFF;
        set[3] ^= 0xFF;
        assert_eq!(set_checksum(&set), before);
        set[4] ^= 0x01;
        assert_ne!(set_checksum(&set), before);
    }

    /// The first 11 sectors of the boot region that `newfs_exfat` of macOS
    /// 26 wrote for a volume of 64 MiB, built again from its fields.
    fn macos_boot_region() -> Vec<u8> {
        let mut region = vec![0u8; 11 * 512];
        let head = hex(
            "eb76904558464154202020000000000000000000000000000000000000000000\
             0000000000000000000000000000000000000000000000000000000000000000\
             00000000000000000000020000000000800000008000000000010000e03f0000\
             050000006a0eb36a00010000090301800000000000000000",
        );
        region[..head.len()].copy_from_slice(&head);
        region[120..510].fill(0xF4);
        region[510] = 0x55;
        region[511] = 0xAA;
        for sector in 1..9 {
            let end = (sector + 1) * 512;
            region[end - 2] = 0x55;
            region[end - 1] = 0xAA;
        }
        region
    }

    #[test]
    fn the_checksum_of_a_boot_region_is_the_one_that_macos_wrote() {
        assert_eq!(boot_checksum(&macos_boot_region()), 0x411E_F78F);
    }

    #[test]
    fn the_checksum_of_a_boot_region_skips_the_flags_and_the_percent_in_use() {
        let mut region = macos_boot_region();
        region[106] = 0x02;
        region[107] = 0x01;
        region[112] = 57;
        assert_eq!(boot_checksum(&region), 0x411E_F78F);
        region[108] = 12;
        assert_ne!(boot_checksum(&region), 0x411E_F78F);
    }

    #[test]
    fn the_checksum_of_a_table_turns_before_each_byte() {
        assert_eq!(table_checksum(&[]), 0);
        assert_eq!(table_checksum(&[1]), 1);
        assert_eq!(table_checksum(&[1, 0]), 0x8000_0000);
        assert_eq!(table_checksum(&[1, 0, 2]), 0x4000_0002);
    }
}
