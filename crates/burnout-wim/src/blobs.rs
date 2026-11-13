//! The offset table of a WIM file, which lists each stream by its SHA-1.
//!
//! Microsoft calls it the lookup table, and wimlib calls it the blob table.

use std::sync::Arc;

use burnout_core::{Error, Result};

use crate::header::Resource;
use crate::sha1::Digest;

/// The bytes of one entry of the table.
pub const ENTRY_BYTES: usize = 50;

/// The size before compression that an entry gives for a solid resource
/// itself, and not for a stream in it.
pub const SOLID_RESOURCE: u64 = 0x1_0000_0000;

/// One stream of a WIM file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub resource: Resource,
    /// The part of a split image that holds the stream, from 1.
    pub part: u16,
    /// How many directory entries name the stream.
    pub references: u32,
    pub hash: Digest,
    /// The solid resources of the file in the order of the table, for a
    /// stream in one of them, or none for a stream that has a resource of
    /// its own. Their bytes after decompression count as one run, one
    /// resource after the other. `resource.offset` is where the stream
    /// starts in that run, and `resource.stored_bytes` is its size.
    pub solid: Option<Arc<[Resource]>>,
}

impl Blob {
    /// The bytes of the stream after decompression.
    pub fn bytes(&self) -> u64 {
        match self.solid {
            Some(_) => self.resource.stored_bytes,
            None => self.resource.original_bytes,
        }
    }

    /// The metadata resource of an edition: its security data and its tree.
    pub fn is_metadata(&self) -> bool {
        self.resource.flags & Resource::METADATA != 0
    }
}

/// Read the entries of the table from its decompressed bytes. `name` names
/// the file in an error.
///
/// The entries of solid resources are not streams, so the list leaves them
/// out. A stream in a solid resource gives its place in the bytes of all
/// solid resources of the file, one after the other. An install.esd of
/// Microsoft lists its streams in the order of that place, and the entry of a
/// resource can come after the first stream in it.
pub fn parse_blobs(bytes: &[u8], name: &str) -> Result<Vec<Blob>> {
    let fault = |detail: String| Error::Image {
        path: name.to_string(),
        detail,
    };
    if bytes.len() % ENTRY_BYTES != 0 {
        return Err(fault(format!(
            "its offset table holds {} bytes, which is not a whole number of entries of {ENTRY_BYTES}",
            bytes.len()
        )));
    }
    let is_solid_resource =
        |r: &Resource| r.flags & Resource::SOLID != 0 && r.original_bytes == SOLID_RESOURCE;
    let entries: Vec<Resource> = bytes
        .chunks_exact(ENTRY_BYTES)
        .map(|e| Resource::parse(&e[..24]))
        .collect();
    let solid: Vec<Resource> = entries.iter().copied().filter(is_solid_resource).collect();
    let shared: Option<Arc<[Resource]>> = (!solid.is_empty()).then(|| Arc::from(solid));
    let mut blobs = Vec::new();
    for (e, resource) in bytes.chunks_exact(ENTRY_BYTES).zip(entries) {
        if is_solid_resource(&resource) {
            continue;
        }
        let solid = if resource.flags & Resource::SOLID != 0 {
            let run = shared.clone().ok_or_else(|| {
                fault(
                    "its offset table names a stream in a solid resource, and no solid resource"
                        .to_string(),
                )
            })?;
            Some(run)
        } else {
            None
        };
        blobs.push(Blob {
            resource,
            part: u16::from_le_bytes([e[24], e[25]]),
            references: u32::from_le_bytes(e[26..30].try_into().unwrap()),
            hash: e[30..50].try_into().unwrap(),
            solid,
        });
    }
    Ok(blobs)
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
    fn a_stream_in_a_solid_resource_has_all_solid_resources_of_the_file() {
        // As an install.esd lists them: a stream can come before the entry
        // of its resource, and the streams follow the order of their place.
        let solid = Resource::SOLID | Resource::COMPRESSED;
        let bytes = [
            entry(10, Resource::SOLID, 0, 0, [1; 20]),
            entry(4000, solid, 208, SOLID_RESOURCE, [0; 20]),
            entry(20, Resource::SOLID, 10, 0, [2; 20]),
            entry(50, 0, 7208, 50, [3; 20]),
            entry(3000, solid, 4208, SOLID_RESOURCE, [0; 20]),
            entry(30, Resource::SOLID, 9000, 0, [4; 20]),
        ]
        .concat();
        let blobs = parse_blobs(&bytes, "a.esd").unwrap();
        let hashes: Vec<u8> = blobs.iter().map(|b| b.hash[0]).collect();
        assert_eq!(hashes, [1, 2, 3, 4]);
        let run = blobs[0].solid.as_ref().unwrap();
        assert_eq!(
            run.iter().map(|r| r.offset).collect::<Vec<_>>(),
            [208, 4208]
        );
        assert!(Arc::ptr_eq(run, blobs[3].solid.as_ref().unwrap()));
        assert_eq!((blobs[1].bytes(), blobs[1].resource.offset), (20, 10));
        assert!(blobs[2].solid.is_none());
        assert_eq!(blobs[2].bytes(), 50);
    }

    #[test]
    fn a_stream_in_a_solid_resource_needs_a_solid_resource() {
        let bytes = entry(10, Resource::SOLID, 0, 0, [1; 20]);
        let e = parse_blobs(&bytes, "a.esd").unwrap_err();
        assert!(e.to_string().contains("no solid resource"), "{e}");
    }

    #[test]
    fn a_table_of_part_of_an_entry_is_refused() {
        let e = parse_blobs(&[0; 51], "a.wim").unwrap_err();
        assert!(e.to_string().contains("51 bytes"), "{e}");
    }
}
