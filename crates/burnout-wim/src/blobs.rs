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
    /// The solid resources that hold the stream, one after the other, or
    /// none for a stream that has a resource of its own. For a stream in
    /// them, `resource.offset` is where the stream starts in their bytes
    /// after decompression, and `resource.stored_bytes` is its size.
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
/// A run of entries for solid resources comes before the entries of the
/// streams in them, as wimlib and Windows write it. The entries of the
/// resources are not streams, so the list leaves them out.
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
    let mut blobs = Vec::new();
    // The run of solid resources that the entries after it use.
    let mut run: Vec<Resource> = Vec::new();
    let mut shared: Option<Arc<[Resource]>> = None;
    for e in bytes.chunks_exact(ENTRY_BYTES) {
        let resource = Resource::parse(&e[..24]);
        let solid = resource.flags & Resource::SOLID != 0;
        if solid && resource.original_bytes == SOLID_RESOURCE {
            if shared.take().is_some() {
                run.clear();
            }
            run.push(resource);
            continue;
        }
        let solid = if solid {
            if shared.is_none() {
                if run.is_empty() {
                    return Err(fault(
                        "its offset table names a stream in a solid resource before any solid resource".to_string(),
                    ));
                }
                shared = Some(Arc::from(std::mem::take(&mut run)));
            }
            shared.clone()
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
    fn the_streams_after_a_run_of_solid_resources_are_in_that_run() {
        let solid = Resource::SOLID | Resource::COMPRESSED;
        let bytes = [
            entry(4000, solid, 208, SOLID_RESOURCE, [0; 20]),
            entry(3000, solid, 4208, SOLID_RESOURCE, [0; 20]),
            entry(10, Resource::SOLID, 0, 0, [1; 20]),
            entry(20, Resource::SOLID, 10, 0, [2; 20]),
            entry(50, 0, 7208, 50, [3; 20]),
            entry(900, solid, 7258, SOLID_RESOURCE, [0; 20]),
            entry(30, Resource::SOLID, 0, 0, [4; 20]),
        ]
        .concat();
        let blobs = parse_blobs(&bytes, "a.esd").unwrap();
        let hashes: Vec<u8> = blobs.iter().map(|b| b.hash[0]).collect();
        assert_eq!(hashes, [1, 2, 3, 4]);
        let first = blobs[0].solid.as_ref().unwrap();
        assert_eq!(
            first.iter().map(|r| r.offset).collect::<Vec<_>>(),
            [208, 4208]
        );
        assert!(Arc::ptr_eq(first, blobs[1].solid.as_ref().unwrap()));
        assert_eq!((blobs[1].bytes(), blobs[1].resource.offset), (20, 10));
        assert!(blobs[2].solid.is_none());
        assert_eq!(blobs[2].bytes(), 50);
        let last = blobs[3].solid.as_ref().unwrap();
        assert_eq!(last.iter().map(|r| r.offset).collect::<Vec<_>>(), [7258]);
    }

    #[test]
    fn a_stream_in_a_solid_resource_needs_the_resource_first() {
        let bytes = entry(10, Resource::SOLID, 0, 0, [1; 20]);
        let e = parse_blobs(&bytes, "a.esd").unwrap_err();
        assert!(e.to_string().contains("before any solid resource"), "{e}");
    }

    #[test]
    fn a_table_of_part_of_an_entry_is_refused() {
        let e = parse_blobs(&[0; 51], "a.wim").unwrap_err();
        assert!(e.to_string().contains("51 bytes"), "{e}");
    }
}
