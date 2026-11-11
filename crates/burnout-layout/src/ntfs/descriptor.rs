//! The security descriptors that the system files of a new volume get.
//!
//! A descriptor here is self-relative: a header, the DACL, the owner and
//! the group, in that order, as format of Windows 11 25H2 writes them.

/// LocalSystem, S-1-5-18.
const SYSTEM: &[u32] = &[18];
/// The group of administrators, S-1-5-32-544.
const ADMINISTRATORS: &[u32] = &[32, 544];

/// Read, and the rights that a read needs.
pub(crate) const READ: u32 = 0x0012_0089;
/// Read and write, without a change of the descriptor or the owner.
pub(crate) const READ_WRITE: u32 = 0x0012_019F;

/// The bytes of a SID in the NT authority, S-1-5-...
fn sid(subs: &[u32]) -> Vec<u8> {
    let mut b = vec![1, subs.len() as u8, 0, 0, 0, 0, 0, 5];
    for s in subs {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

/// A descriptor with SYSTEM as the owner, the administrators as the group,
/// and a DACL that allows `mask` to both.
pub(crate) fn system_descriptor(mask: u32) -> Vec<u8> {
    let mut aces = Vec::new();
    for who in [SYSTEM, ADMINISTRATORS] {
        let s = sid(who);
        let size = 8 + s.len();
        aces.push(0u8); // ACCESS_ALLOWED
        aces.push(0); // no inheritance
        aces.extend_from_slice(&(size as u16).to_le_bytes());
        aces.extend_from_slice(&mask.to_le_bytes());
        aces.extend_from_slice(&s);
    }
    let mut acl = vec![2u8, 0];
    acl.extend_from_slice(&((8 + aces.len()) as u16).to_le_bytes());
    acl.extend_from_slice(&2u16.to_le_bytes());
    acl.extend_from_slice(&[0, 0]);
    acl.extend_from_slice(&aces);

    let (owner, group) = (sid(SYSTEM), sid(ADMINISTRATORS));
    let dacl_at = 20u32;
    let owner_at = dacl_at + acl.len() as u32;
    let group_at = owner_at + owner.len() as u32;
    let mut b = vec![1u8, 0];
    // Self-relative, with a DACL present.
    b.extend_from_slice(&0x8004u16.to_le_bytes());
    b.extend_from_slice(&owner_at.to_le_bytes());
    b.extend_from_slice(&group_at.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&dacl_at.to_le_bytes());
    b.extend_from_slice(&acl);
    b.extend_from_slice(&owner);
    b.extend_from_slice(&group);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_descriptors_are_the_ones_that_windows_writes() {
        // Security IDs 256 and 257 of a volume that format of Windows 11 25H2
        // wrote on 2026-11-11.
        let read = "01000480480000005400000000000000140000000200340002000000000014008900120001010000000000051200000000001800890012000102000000000005200000002002000001010000000000051200000001020000000000052000000020020000";
        let read_write = "01000480480000005400000000000000140000000200340002000000000014009f011200010100000000000512000000000018009f0112000102000000000005200000002002000001010000000000051200000001020000000000052000000020020000";
        assert_eq!(system_descriptor(READ), hex(read));
        assert_eq!(system_descriptor(READ_WRITE), hex(read_write));
    }
}
