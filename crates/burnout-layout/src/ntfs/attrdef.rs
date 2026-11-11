//! The value of `$AttrDef`: the definition of each type of attribute.
//!
//! Each definition takes 160 bytes: the name in 64 UTF-16 units, the type,
//! the display rule, the collation rule, the flags, and the smallest and the
//! largest size. The file holds 16 definitions, and the unused ones are zero.
//! The values are the ones that `format` of Windows 11 25H2 wrote, measured
//! on 2026-11-11.

use crate::ntfs::record::kind;

/// The bytes of the file.
pub(crate) const ATTRDEF_BYTES: usize = 2560;
const ENTRY_BYTES: usize = 160;

/// The attribute is always resident.
const RESIDENT: u32 = 0x40;
/// The attribute can be indexed.
const INDEXABLE: u32 = 0x02;
/// A change to the attribute is logged, even when it is not resident.
const LOG_NON_RESIDENT: u32 = 0x80;

/// The name, the type, the flags, and the smallest and largest size, in
/// the order of the type.
const DEFINITIONS: [(&str, u32, u32, i64, i64); 15] = [
    (
        "$STANDARD_INFORMATION",
        kind::STANDARD_INFORMATION,
        RESIDENT,
        48,
        72,
    ),
    (
        "$ATTRIBUTE_LIST",
        kind::ATTRIBUTE_LIST,
        LOG_NON_RESIDENT,
        0,
        -1,
    ),
    ("$FILE_NAME", kind::FILE_NAME, RESIDENT | INDEXABLE, 68, 578),
    ("$OBJECT_ID", kind::OBJECT_ID, RESIDENT, 0, 256),
    (
        "$SECURITY_DESCRIPTOR",
        kind::SECURITY_DESCRIPTOR,
        LOG_NON_RESIDENT,
        0,
        -1,
    ),
    ("$VOLUME_NAME", kind::VOLUME_NAME, RESIDENT, 2, 256),
    (
        "$VOLUME_INFORMATION",
        kind::VOLUME_INFORMATION,
        RESIDENT,
        12,
        12,
    ),
    ("$DATA", kind::DATA, 0, 0, -1),
    ("$INDEX_ROOT", kind::INDEX_ROOT, RESIDENT, 0, -1),
    (
        "$INDEX_ALLOCATION",
        kind::INDEX_ALLOCATION,
        LOG_NON_RESIDENT,
        0,
        -1,
    ),
    ("$BITMAP", kind::BITMAP, LOG_NON_RESIDENT, 0, -1),
    (
        "$REPARSE_POINT",
        kind::REPARSE_POINT,
        LOG_NON_RESIDENT,
        0,
        16384,
    ),
    ("$EA_INFORMATION", kind::EA_INFORMATION, RESIDENT, 8, 8),
    ("$EA", kind::EA, 0, 0, 65536),
    (
        "$LOGGED_UTILITY_STREAM",
        kind::LOGGED_UTILITY_STREAM,
        LOG_NON_RESIDENT,
        0,
        65536,
    ),
];

/// The bytes of `$AttrDef`.
pub(crate) fn attrdef() -> Vec<u8> {
    let mut out = vec![0u8; ATTRDEF_BYTES];
    for (i, (name, kind, flags, min, max)) in DEFINITIONS.iter().enumerate() {
        let e = &mut out[i * ENTRY_BYTES..(i + 1) * ENTRY_BYTES];
        for (j, u) in name.encode_utf16().enumerate() {
            e[2 * j..2 * j + 2].copy_from_slice(&u.to_le_bytes());
        }
        e[0x80..0x84].copy_from_slice(&kind.to_le_bytes());
        e[0x8C..0x90].copy_from_slice(&flags.to_le_bytes());
        e[0x90..0x98].copy_from_slice(&min.to_le_bytes());
        e[0x98..0xA0].copy_from_slice(&max.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_definition_has_its_name_type_flags_and_sizes() {
        let b = attrdef();
        assert_eq!(b.len(), 2560);
        let entry = |i: usize| &b[i * 160..(i + 1) * 160];
        let name = |e: &[u8]| {
            let units: Vec<u16> = e[..128]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|u| *u != 0)
                .collect();
            String::from_utf16(&units).unwrap()
        };
        let fn_entry = entry(2);
        assert_eq!(name(fn_entry), "$FILE_NAME");
        assert_eq!(&fn_entry[0x80..0x84], &0x30u32.to_le_bytes());
        assert_eq!(&fn_entry[0x88..0x8C], &0u32.to_le_bytes(), "collation 0");
        assert_eq!(&fn_entry[0x8C..0x90], &0x42u32.to_le_bytes());
        assert_eq!(&fn_entry[0x90..0x98], &68i64.to_le_bytes());
        assert_eq!(&fn_entry[0x98..0xA0], &578i64.to_le_bytes());
        let last = entry(14);
        assert_eq!(name(last), "$LOGGED_UTILITY_STREAM");
        assert_eq!(&last[0x80..0x84], &0x100u32.to_le_bytes());
        assert_eq!(&last[0x98..0xA0], &65536i64.to_le_bytes());
        assert!(entry(15).iter().all(|b| *b == 0));
        // The types go up.
        let kinds: Vec<u32> = (0..15)
            .map(|i| u32::from_le_bytes(entry(i)[0x80..0x84].try_into().unwrap()))
            .collect();
        assert!(kinds.windows(2).all(|w| w[0] < w[1]));
    }
}
