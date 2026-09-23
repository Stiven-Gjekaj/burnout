//! The tag at the start of each descriptor of UDF.
//!
//! A tag says which descriptor follows, where it was written, and the CRC of
//! the bytes after it. A reader checks all three before it trusts a
//! descriptor. A block of zeros, a copy at the wrong place or a torn write
//! then shows up as a fault, and not as a tree with holes in it.

/// The bytes of a tag.
pub(crate) const TAG: usize = 16;

/// The identifiers of the descriptors that Burnout reads.
pub(crate) const PRIMARY_VOLUME: u16 = 1;
pub(crate) const ANCHOR: u16 = 2;
pub(crate) const POINTER: u16 = 3;
pub(crate) const IMPLEMENTATION_USE: u16 = 4;
pub(crate) const PARTITION: u16 = 5;
pub(crate) const LOGICAL_VOLUME: u16 = 6;
pub(crate) const UNALLOCATED_SPACE: u16 = 7;
pub(crate) const TERMINATING: u16 = 8;
pub(crate) const FILE_SET: u16 = 256;
pub(crate) const FILE_IDENTIFIER: u16 = 257;
pub(crate) const ALLOCATION_EXTENT: u16 = 258;
pub(crate) const INDIRECT_ENTRY: u16 = 259;
pub(crate) const FILE_ENTRY: u16 = 261;
pub(crate) const EXTENDED_FILE_ENTRY: u16 = 266;

/// The name of a descriptor, for a message.
pub(crate) fn name(id: u16) -> String {
    let name = match id {
        PRIMARY_VOLUME => "a primary volume descriptor",
        ANCHOR => "an anchor volume descriptor pointer",
        POINTER => "a volume descriptor pointer",
        IMPLEMENTATION_USE => "an implementation use volume descriptor",
        PARTITION => "a partition descriptor",
        LOGICAL_VOLUME => "a logical volume descriptor",
        UNALLOCATED_SPACE => "an unallocated space descriptor",
        TERMINATING => "a terminating descriptor",
        FILE_SET => "a file set descriptor",
        FILE_IDENTIFIER => "a file identifier descriptor",
        ALLOCATION_EXTENT => "an allocation extent descriptor",
        INDIRECT_ENTRY => "an indirect entry",
        FILE_ENTRY => "a file entry",
        EXTENDED_FILE_ENTRY => "an extended file entry",
        _ => return format!("a descriptor with the identifier {id}"),
    };
    name.to_string()
}

/// What a tag says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tag {
    pub id: u16,
    pub version: u16,
    pub serial: u16,
    /// The block that the descriptor says it is in.
    pub location: u32,
    /// The bytes after the tag that the CRC covers.
    pub crc_length: u16,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// The CRC of UDF, which is CRC-ITU-T: the polynomial x^16 + x^12 + x^5 + 1,
/// with a start of zero.
pub(crate) fn crc_itu_t(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = match crc & 0x8000 {
                0 => crc << 1,
                _ => (crc << 1) ^ 0x1021,
            };
        }
    }
    crc
}

/// The tag at the start of `bytes`, which were read from block `location`,
/// with each check that it allows.
pub(crate) fn read_tag(bytes: &[u8], location: u32) -> Result<Tag, String> {
    let Some(tag) = bytes.get(..TAG) else {
        return Err(format!("{} bytes are too few for a tag", bytes.len()));
    };
    let sum = tag
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != 4)
        .fold(0u8, |sum, (_, b)| sum.wrapping_add(*b));
    if sum != tag[4] {
        return Err(format!(
            "no tag is at block {location}: the bytes of a tag sum to {sum:#04x}, and the tag says {:#04x}",
            tag[4]
        ));
    }
    let found = Tag {
        id: u16_at(tag, 0),
        version: u16_at(tag, 2),
        serial: u16_at(tag, 6),
        location: u32::from_le_bytes(tag[12..16].try_into().unwrap()),
        crc_length: u16_at(tag, 10),
    };
    if found.version != 2 && found.version != 3 {
        return Err(format!(
            "the tag at block {location} has version {}, and UDF writes 2 or 3",
            found.version
        ));
    }
    if found.location != location {
        return Err(format!(
            "{} says that it is at block {}, and it is at block {location}",
            name(found.id),
            found.location
        ));
    }
    let end = TAG + found.crc_length as usize;
    let Some(body) = bytes.get(TAG..end) else {
        return Err(format!(
            "the CRC of {} at block {location} covers {} bytes, and only {} follow the tag",
            name(found.id),
            found.crc_length,
            bytes.len() - TAG
        ));
    };
    let crc = crc_itu_t(body);
    if crc != u16_at(tag, 8) {
        return Err(format!(
            "the CRC of {} at block {location} is {:#06x}, and its bytes give {crc:#06x}",
            name(found.id),
            u16_at(tag, 8)
        ));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tagged;

    #[test]
    fn the_crc_is_crc_itu_t() {
        // The example of ECMA-167, and the check value of the CRC.
        assert_eq!(crc_itu_t(&[0x70, 0x6A, 0x77]), 0x3299);
        assert_eq!(crc_itu_t(b"123456789"), 0x31C3);
    }

    #[test]
    fn the_anchor_of_a_windows_iso_passes() {
        // The first 32 bytes at block 256 of Win11_25H2_English_x64_v2.iso.
        // The 480 bytes after them are zero.
        let mut anchor = vec![
            0x02, 0x00, 0x02, 0x00, 0x74, 0x00, 0x00, 0x00, 0x18, 0x66, 0xF0, 0x01, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00,
            0x13, 0x01, 0x00, 0x00,
        ];
        anchor.resize(512, 0);
        let tag = read_tag(&anchor, 256).unwrap();
        assert_eq!(
            tag,
            Tag {
                id: ANCHOR,
                version: 2,
                serial: 0,
                location: 256,
                crc_length: 496,
            }
        );
    }

    #[test]
    fn a_tag_that_the_builder_writes_passes() {
        let d = tagged(FILE_ENTRY, 40, b"some bytes of a body");
        let tag = read_tag(&d, 40).unwrap();
        assert_eq!((tag.id, tag.location, tag.crc_length), (FILE_ENTRY, 40, 20));
    }

    #[test]
    fn a_wrong_checksum_is_refused() {
        let mut d = tagged(FILE_ENTRY, 40, b"body");
        d[4] ^= 1;
        assert!(read_tag(&d, 40)
            .unwrap_err()
            .contains("no tag is at block 40"));
    }

    #[test]
    fn a_block_of_zeros_is_refused() {
        let e = read_tag(&[0; 2048], 7).unwrap_err();
        assert!(e.contains("has version 0"), "{e}");
    }

    #[test]
    fn a_descriptor_at_the_wrong_block_is_refused() {
        let d = tagged(FILE_ENTRY, 40, b"body");
        let e = read_tag(&d, 41).unwrap_err();
        assert!(
            e.contains("a file entry says that it is at block 40"),
            "{e}"
        );
    }

    #[test]
    fn a_changed_byte_of_the_body_is_refused() {
        let mut d = tagged(FILE_SET, 0, b"body");
        d[17] ^= 0x20;
        let e = read_tag(&d, 0).unwrap_err();
        assert!(e.contains("the CRC of a file set descriptor"), "{e}");
    }

    #[test]
    fn a_crc_past_the_end_of_the_bytes_is_refused() {
        let d = tagged(FILE_SET, 0, b"body");
        let e = read_tag(&d[..18], 0).unwrap_err();
        assert!(e.contains("covers 4 bytes, and only 2 follow"), "{e}");
        assert!(read_tag(&d[..10], 0).unwrap_err().contains("too few"));
    }
}
