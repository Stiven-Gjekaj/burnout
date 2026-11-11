//! The header of a WIM file, which is its first 208 bytes.
//!
//! An ESD file is a WIM file with LZMS compression, so the same header
//! starts it.

use burnout_core::{Error, Result};

/// The size of the header, which the header also records.
pub const HEADER_BYTES: usize = 208;

const MAGIC: &[u8; 8] = b"MSWIM\0\0\0";

const FLAG_COMPRESSED: u32 = 0x0000_0002;
const FLAG_XPRESS: u32 = 0x0002_0000;
const FLAG_LZX: u32 = 0x0004_0000;
const FLAG_LZMS: u32 = 0x0008_0000;
const FLAG_XPRESS_HUFFMAN: u32 = 0x0020_0000;

/// The compression of the resources of a WIM file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Xpress,
    Lzx,
    Lzms,
}

impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Xpress => "XPRESS",
            Compression::Lzx => "LZX",
            Compression::Lzms => "LZMS",
        }
    }
}

/// Where a resource is in the file, and its size before and after
/// compression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The bytes that the file holds for the resource.
    pub stored_bytes: u64,
    pub flags: u8,
    /// The first byte, counted from the start of the file.
    pub offset: u64,
    /// The bytes of the resource after it is decompressed.
    pub original_bytes: u64,
}

impl Resource {
    pub const FREE: u8 = 0x01;
    pub const METADATA: u8 = 0x02;
    pub const COMPRESSED: u8 = 0x04;
    pub const SPANNED: u8 = 0x08;
    pub const SOLID: u8 = 0x10;

    fn parse(b: &[u8]) -> Resource {
        let mut size = [0u8; 8];
        size[..7].copy_from_slice(&b[..7]);
        Resource {
            stored_bytes: u64::from_le_bytes(size),
            flags: b[7],
            offset: u64_at(b, 8),
            original_bytes: u64_at(b, 16),
        }
    }

    pub fn is_compressed(&self) -> bool {
        self.flags & Resource::COMPRESSED != 0
    }
}

/// The header of a WIM file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub flags: u32,
    /// The size of one chunk of a compressed resource.
    pub chunk_bytes: u32,
    /// The part that this file is, and the parts that the image has. A
    /// split image (`.swm`) has more than one.
    pub part: u16,
    pub parts: u16,
    pub image_count: u32,
    pub offset_table: Resource,
    pub xml: Resource,
    pub boot_metadata: Resource,
    /// The image that Windows PE starts, or 0 for none.
    pub boot_index: u32,
    pub integrity: Resource,
}

impl Header {
    /// Read the header from the first bytes of a file. `name` names the
    /// file in an error.
    pub fn parse(b: &[u8], name: &str) -> Result<Header> {
        let fault = |detail: String| Error::Image {
            path: name.to_string(),
            detail,
        };
        if b.len() < HEADER_BYTES || &b[..8] != MAGIC {
            return Err(fault("it does not start as a WIM file".to_string()));
        }
        let size = u32_at(b, 8);
        if size as usize != HEADER_BYTES {
            return Err(fault(format!(
                "its WIM header says that it holds {size} bytes, and a WIM header holds {HEADER_BYTES}"
            )));
        }
        let header = Header {
            version: u32_at(b, 0x0C),
            flags: u32_at(b, 0x10),
            chunk_bytes: u32_at(b, 0x14),
            part: u16_at(b, 0x28),
            parts: u16_at(b, 0x2A),
            image_count: u32_at(b, 0x2C),
            offset_table: Resource::parse(&b[0x30..0x48]),
            xml: Resource::parse(&b[0x48..0x60]),
            boot_metadata: Resource::parse(&b[0x60..0x78]),
            boot_index: u32_at(b, 0x78),
            integrity: Resource::parse(&b[0x7C..0x94]),
        };
        if header.part == 0 || header.part > header.parts {
            return Err(fault(format!(
                "its WIM header says that it is part {} of {}",
                header.part, header.parts
            )));
        }
        Ok(header)
    }

    /// The compression of the resources, from the flags.
    pub fn compression(&self) -> Compression {
        if self.flags & FLAG_COMPRESSED == 0 {
            Compression::None
        } else if self.flags & FLAG_LZMS != 0 {
            Compression::Lzms
        } else if self.flags & FLAG_LZX != 0 {
            Compression::Lzx
        } else if self.flags & (FLAG_XPRESS | FLAG_XPRESS_HUFFMAN) != 0 {
            Compression::Xpress
        } else {
            Compression::None
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A header with these values, as the tests of this crate need it.
    pub(crate) fn header(flags: u32, image_count: u32, xml: (u64, u64)) -> Vec<u8> {
        let mut b = vec![0u8; HEADER_BYTES];
        b[..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
        b[0x0C..0x10].copy_from_slice(&0x10d00u32.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&flags.to_le_bytes());
        b[0x14..0x18].copy_from_slice(&32768u32.to_le_bytes());
        b[0x28..0x2A].copy_from_slice(&1u16.to_le_bytes());
        b[0x2A..0x2C].copy_from_slice(&1u16.to_le_bytes());
        b[0x2C..0x30].copy_from_slice(&image_count.to_le_bytes());
        let (offset, bytes) = xml;
        b[0x48..0x4F].copy_from_slice(&bytes.to_le_bytes()[..7]);
        b[0x50..0x58].copy_from_slice(&offset.to_le_bytes());
        b[0x58..0x60].copy_from_slice(&bytes.to_le_bytes());
        b[0x78..0x7C].copy_from_slice(&2u32.to_le_bytes());
        b
    }

    #[test]
    fn a_header_gives_its_fields() {
        let b = header(FLAG_COMPRESSED | FLAG_LZX, 4, (1_000_000, 4_000));
        let h = Header::parse(&b, "install.wim").unwrap();
        assert_eq!(h.image_count, 4);
        assert_eq!(h.chunk_bytes, 32768);
        assert_eq!((h.part, h.parts), (1, 1));
        assert_eq!(h.boot_index, 2);
        assert_eq!(h.xml.offset, 1_000_000);
        assert_eq!(h.xml.stored_bytes, 4_000);
        assert_eq!(h.xml.original_bytes, 4_000);
        assert!(!h.xml.is_compressed());
        assert_eq!(h.compression(), Compression::Lzx);
    }

    #[test]
    fn the_flags_give_the_compression() {
        for (flags, compression) in [
            (0, Compression::None),
            (FLAG_LZX, Compression::None),
            (FLAG_COMPRESSED | FLAG_XPRESS, Compression::Xpress),
            (FLAG_COMPRESSED | FLAG_XPRESS_HUFFMAN, Compression::Xpress),
            (FLAG_COMPRESSED | FLAG_LZX, Compression::Lzx),
            (FLAG_COMPRESSED | FLAG_LZMS, Compression::Lzms),
        ] {
            let h = Header::parse(&header(flags, 1, (208, 0)), "a.wim").unwrap();
            assert_eq!(h.compression(), compression, "flags {flags:#x}");
        }
    }

    #[test]
    fn a_file_that_is_not_a_wim_is_refused() {
        let e = Header::parse(&[0u8; HEADER_BYTES], "a.wim").unwrap_err();
        assert!(
            e.to_string().contains("does not start as a WIM file"),
            "{e}"
        );
        let e = Header::parse(&header(0, 1, (0, 0))[..100], "a.wim").unwrap_err();
        assert!(
            e.to_string().contains("does not start as a WIM file"),
            "{e}"
        );

        let mut b = header(0, 1, (0, 0));
        b[8] = 200;
        let e = Header::parse(&b, "a.wim").unwrap_err();
        assert!(e.to_string().contains("holds 200 bytes"), "{e}");

        let mut b = header(0, 1, (0, 0));
        b[0x28] = 3;
        let e = Header::parse(&b, "a.wim").unwrap_err();
        assert!(e.to_string().contains("part 3 of 1"), "{e}");
    }
}
