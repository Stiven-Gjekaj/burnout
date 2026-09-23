//! UDF, the file system that holds the files of a Windows ISO.
//!
//! The ISO 9660 side of a Windows ISO holds only a note that tells a person
//! to use a reader of UDF. UDF finds its volume from an anchor at block 256,
//! then a sequence of volume descriptors, a partition and a file set. The
//! root of the file set is the tree.

mod entry;
mod tag;
mod text;
mod volume;
mod walk;

pub(crate) use tag::{crc_itu_t, read_tag, Tag};
pub(crate) use volume::{find_volume, Address, Volume};
pub(crate) use walk::walk;

/// The little-endian numbers that UDF records.
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
