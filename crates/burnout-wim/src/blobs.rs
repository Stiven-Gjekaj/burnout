//! The offset table of a WIM file, which lists each stream by its SHA-1.
//!
//! Microsoft calls it the lookup table, and wimlib calls it the blob table.

use burnout_core::{Error, Result};

use crate::header::Resource;
use crate::sha1::Digest;

/// The bytes of one entry of the table.
pub const ENTRY_BYTES: usize = 50;

/// One stream of a WIM file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub resource: Resource,
    /// The part of a split image that holds the stream, from 1.
    pub part: u16,
    /// How many directory entries name the stream.
    pub references: u32,
    pub hash: Digest,
}

impl Blob {
    /// The metadata resource of an edition: its security data and its tree.
    pub fn is_metadata(&self) -> bool {
        self.resource.flags & Resource::METADATA != 0
    }
}

/// Read the entries of the table from its decompressed bytes. `name` names
/// the file in an error.
pub fn parse_blobs(bytes: &[u8], name: &str) -> Result<Vec<Blob>> {
    if bytes.len() % ENTRY_BYTES != 0 {
        return Err(Error::Image {
            path: name.to_string(),
            detail: format!(
                "its offset table holds {} bytes, which is not a whole number of entries of {ENTRY_BYTES}",
                bytes.len()
            ),
        });
    }
    Ok(bytes
        .chunks_exact(ENTRY_BYTES)
        .map(|e| Blob {
            resource: Resource::parse(&e[..24]),
            part: u16::from_le_bytes([e[24], e[25]]),
            references: u32::from_le_bytes(e[26..30].try_into().unwrap()),
            hash: e[30..50].try_into().unwrap(),
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One entry of the table.
    pub(crate) fn entry(
        stored: u64,
        flags: u8,
        offset: u64,
        original: u64,
        hash: Digest,
    ) -> Vec<u8> {
        let mut e = vec![0u8; ENTRY_BYTES];
        e[..7].copy_from_slice(&stored.to_le_bytes()[..7]);
        e[7] = flags;
        e[8..16].copy_from_slice(&offset.to_le_bytes());
        e[16..24].copy_from_slice(&original.to_le_bytes());
        e[24..26].copy_from_slice(&1u16.to_le_bytes());
        e[26..30].copy_from_slice(&3u32.to_le_bytes());
        e[30..50].copy_from_slice(&hash);
        e
    }

    #[test]
    fn the_entries_give_their_resources_and_hashes() {
        let bytes = [
            entry(100, Resource::COMPRESSED, 208, 300, [1; 20]),
            entry(50, Resource::METADATA, 308, 50, [2; 20]),
        ]
        .concat();
        let blobs = parse_blobs(&bytes, "a.wim").unwrap();
        assert_eq!(blobs.len(), 2);
        assert_eq!(blobs[0].resource.stored_bytes, 100);
        assert_eq!(blobs[0].resource.original_bytes, 300);
        assert!(blobs[0].resource.is_compressed());
        assert!(!blobs[0].is_metadata());
        assert_eq!((blobs[0].part, blobs[0].references), (1, 3));
        assert_eq!(blobs[1].hash, [2; 20]);
        assert!(blobs[1].is_metadata());
    }

    #[test]
    fn a_table_of_part_of_an_entry_is_refused() {
        let e = parse_blobs(&[0; 51], "a.wim").unwrap_err();
        assert!(e.to_string().contains("51 bytes"), "{e}");
    }
}
