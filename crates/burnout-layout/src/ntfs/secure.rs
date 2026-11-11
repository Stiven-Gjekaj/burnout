//! `$Secure`: the security descriptors of a volume, which each file names by
//! its security ID in $STANDARD_INFORMATION.
//!
//! The stream $SDS holds each descriptor once, behind a header of 20 bytes,
//! on a 16-byte boundary. It goes in blocks of 256 KiB, and each block has a
//! copy in the 256 KiB after it. The index $SDH finds a descriptor by its
//! hash, and $SII finds it by its ID. The layout and the hash were checked
//! against the $Secure that format of Windows 11 25H2 wrote, on 2026-11-11.

use crate::ntfs::index::Entry;

/// The first security ID that a descriptor gets.
pub(crate) const FIRST_ID: u32 = 256;
const BLOCK: usize = 0x40000;
const HEADER: usize = 20;

/// The hash of a descriptor: for each 32-bit word, rotate left by 3 and add.
pub(crate) fn sd_hash(sd: &[u8]) -> u32 {
    sd.chunks_exact(4).fold(0u32, |h, w| {
        h.rotate_left(3)
            .wrapping_add(u32::from_le_bytes(w.try_into().unwrap()))
    })
}

/// What `$Secure` holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Secure {
    /// The bytes of $SDS.
    pub sds: Vec<u8>,
    /// The entries of $SDH and of $SII, in the order of their keys.
    pub sdh: Vec<Entry>,
    pub sii: Vec<Entry>,
}

/// Lay out `descriptors`. The first gets ID 256, the next 257, and so on.
pub(crate) fn secure(descriptors: &[Vec<u8>]) -> Secure {
    let mut sds: Vec<u8> = Vec::new();
    let mut sdh = Vec::new();
    let mut sii = Vec::new();
    let mut at = 0usize;
    for (i, sd) in descriptors.iter().enumerate() {
        let id = FIRST_ID + i as u32;
        let len = HEADER + sd.len();
        // An entry does not cross into the copy of its block.
        if at % (2 * BLOCK) + len > BLOCK {
            at = (at / (2 * BLOCK) + 1) * 2 * BLOCK;
        }
        let hash = sd_hash(sd);
        let mut header = Vec::with_capacity(HEADER);
        header.extend_from_slice(&hash.to_le_bytes());
        header.extend_from_slice(&id.to_le_bytes());
        header.extend_from_slice(&(at as u64).to_le_bytes());
        header.extend_from_slice(&(len as u32).to_le_bytes());
        let end = at + len;
        if sds.len() < end + BLOCK {
            sds.resize(end + BLOCK, 0);
        }
        for copy in [at, at + BLOCK] {
            sds[copy..copy + HEADER].copy_from_slice(&header);
            sds[copy + HEADER..copy + len].copy_from_slice(sd);
        }
        let mut sdh_key = hash.to_le_bytes().to_vec();
        sdh_key.extend_from_slice(&id.to_le_bytes());
        sdh.push(Entry {
            file: 0,
            key: sdh_key,
            data: header.clone(),
        });
        sii.push(Entry {
            file: 0,
            key: id.to_le_bytes().to_vec(),
            data: header,
        });
        at = (end + 15) & !15;
    }
    // The stream ends with the last entry of the copy, as on Windows: each
    // entry grew it to its end in the copy.
    let key = |e: &Entry| -> (u32, u32) {
        let w = |i: usize| u32::from_le_bytes(e.key[i..i + 4].try_into().unwrap());
        (w(0), if e.key.len() > 4 { w(4) } else { 0 })
    };
    sdh.sort_by_key(key);
    sii.sort_by_key(key);
    Secure { sds, sdh, sii }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_rotates_and_adds_each_word() {
        assert_eq!(sd_hash(&[]), 0);
        assert_eq!(sd_hash(&1u32.to_le_bytes()), 1);
        let two = [1u32.to_le_bytes(), 2u32.to_le_bytes()].concat();
        assert_eq!(sd_hash(&two), (1 << 3) + 2);
        let wrap = [0x8000_0000u32.to_le_bytes(), 0u32.to_le_bytes()].concat();
        assert_eq!(sd_hash(&wrap), 4);
    }

    #[test]
    fn each_descriptor_has_its_header_its_copy_and_its_two_index_entries() {
        let sds = [vec![1u8; 100], vec![2u8; 96]];
        let s = secure(&sds);
        // The first at 0, the second on the next 16-byte boundary.
        let header = |at: usize| {
            (
                u32::from_le_bytes(s.sds[at..at + 4].try_into().unwrap()),
                u32::from_le_bytes(s.sds[at + 4..at + 8].try_into().unwrap()),
                u64::from_le_bytes(s.sds[at + 8..at + 16].try_into().unwrap()),
                u32::from_le_bytes(s.sds[at + 16..at + 20].try_into().unwrap()),
            )
        };
        assert_eq!(header(0), (sd_hash(&sds[0]), 256, 0, 120));
        assert_eq!(header(0x80), (sd_hash(&sds[1]), 257, 0x80, 116));
        assert_eq!(&s.sds[0x40000..0x40000 + 120], &s.sds[..120]);
        // The stream ends with the last entry of the copy, as on Windows.
        assert_eq!(s.sds.len(), 0x40000 + 0x80 + 116);
        assert_eq!(s.sii.len(), 2);
        assert_eq!(s.sii[0].key, 256u32.to_le_bytes());
        assert_eq!(s.sii[0].data, s.sds[..20]);
        let hashes: Vec<u32> = s
            .sdh
            .iter()
            .map(|e| u32::from_le_bytes(e.key[..4].try_into().unwrap()))
            .collect();
        assert!(hashes.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn an_entry_that_does_not_fit_its_block_starts_the_next_one() {
        let big = vec![vec![7u8; 100_000]; 3];
        let s = secure(&big);
        let offset = |e: &Entry| u64::from_le_bytes(e.data[8..16].try_into().unwrap());
        let offsets: Vec<u64> = s.sii.iter().map(offset).collect();
        assert_eq!(offsets, [0, 100_032, 0x80000]);
        assert_eq!(
            &s.sds[0x80000 + 0x40000..0x80000 + 0x40000 + 20],
            &s.sds[0x80000..0x80000 + 20]
        );
    }
}
