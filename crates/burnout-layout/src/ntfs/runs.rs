//! The data runs of a non-resident NTFS attribute.
//!
//! A run list says where the clusters of an attribute are. Each run starts
//! with a byte whose low half is the size of the length field and whose high
//! half is the size of the offset field. The offset is signed, and counts
//! from the first cluster of the run before. A run with no offset field is
//! sparse: it reads as zeros and holds no cluster. A byte of 0 ends the list.
//!
//! NTFS reads the length as signed too, so a length whose top byte has its
//! high bit set takes one byte more. Windows writes the run of 65,263
//! clusters as `03 EF FE 00`, and reads `02 EF FE` as a negative length: a
//! volume with such a run does not mount, measured on 2026-11-11.

/// One run: its first cluster, or `None` for a sparse run, and its count of
/// clusters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// The smallest count of bytes that holds `value` as a signed number.
fn signed_bytes(value: i64) -> usize {
    (1..=8)
        .find(|&n| {
            let shift = 64 - 8 * n as u32;
            (value << shift) >> shift == value
        })
        .unwrap_or(8)
}

/// The bytes of a run list, with the 0 that ends it.
pub(crate) fn encode(runs: &[Run]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut previous = 0i64;
    for run in runs.iter().filter(|r| r.clusters > 0) {
        let length_bytes = signed_bytes(run.clusters as i64);
        let (offset_bytes, offset) = match run.lcn {
            Some(lcn) => {
                let delta = lcn as i64 - previous;
                previous = lcn as i64;
                (signed_bytes(delta), delta)
            }
            None => (0, 0),
        };
        out.push(((offset_bytes << 4) | length_bytes) as u8);
        out.extend_from_slice(&run.clusters.to_le_bytes()[..length_bytes]);
        out.extend_from_slice(&offset.to_le_bytes()[..offset_bytes]);
    }
    out.push(0);
    out
}

/// The runs of a run list, or `None` for a list that is damaged.
pub(crate) fn decode(bytes: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut at = 0;
    let mut lcn = 0i64;
    loop {
        let head = *bytes.get(at)?;
        if head == 0 {
            return Some(runs);
        }
        let length_bytes = (head & 0x0F) as usize;
        let offset_bytes = (head >> 4) as usize;
        if length_bytes == 0 || length_bytes > 8 || offset_bytes > 8 {
            return None;
        }
        let field = bytes.get(at + 1..at + 1 + length_bytes + offset_bytes)?;
        let mut length = [0u8; 8];
        length[..length_bytes].copy_from_slice(&field[..length_bytes]);
        let clusters = u64::from_le_bytes(length);
        let run_lcn = if offset_bytes == 0 {
            None
        } else {
            let raw = &field[length_bytes..];
            let fill = if raw[offset_bytes - 1] & 0x80 != 0 {
                0xFF
            } else {
                0
            };
            let mut offset = [fill; 8];
            offset[..offset_bytes].copy_from_slice(raw);
            lcn = lcn.checked_add(i64::from_le_bytes(offset))?;
            Some(u64::try_from(lcn).ok()?)
        };
        runs.push(Run {
            lcn: run_lcn,
            clusters,
        });
        at += 1 + length_bytes + offset_bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lcn: u64, clusters: u64) -> Run {
        Run {
            lcn: Some(lcn),
            clusters,
        }
    }

    #[test]
    fn a_run_list_has_the_bytes_that_the_format_gives() {
        // 0x18 clusters at cluster 0x5634: length in 1 byte, offset in 2.
        assert_eq!(encode(&[run(0x5634, 0x18)]), [0x21, 0x18, 0x34, 0x56, 0]);
        // The second run is 0x10 clusters back: a negative offset.
        assert_eq!(
            encode(&[run(0x100, 4), run(0xF0, 2)]),
            [0x21, 0x04, 0x00, 0x01, 0x11, 0x02, 0xF0, 0]
        );
        // A run at cluster 0x80 needs a second byte, so the offset is not
        // read as negative.
        assert_eq!(encode(&[run(0x80, 1)]), [0x21, 0x01, 0x80, 0x00, 0]);
        // A sparse run has no offset.
        let sparse = Run {
            lcn: None,
            clusters: 0x300,
        };
        assert_eq!(encode(&[sparse]), [0x02, 0x00, 0x03, 0]);
        // A length with the high bit of its top byte set takes one byte
        // more, as Windows writes the run of $Bad.
        let bad = Run {
            lcn: None,
            clusters: 0xFEEF,
        };
        assert_eq!(encode(&[bad]), [0x03, 0xEF, 0xFE, 0x00, 0]);
        assert_eq!(encode(&[run(5, 0x80)]), [0x12, 0x80, 0x00, 0x05, 0]);
        assert_eq!(encode(&[]), [0]);
    }

    #[test]
    fn each_list_decodes_to_its_runs() {
        let lists = [
            vec![run(0x5634, 0x18)],
            vec![run(0x100, 4), run(0xF0, 2), run(0x1_0000_0000, 7)],
            vec![
                run(10, 1),
                Run {
                    lcn: None,
                    clusters: 5,
                },
                run(3, 1),
            ],
            vec![run(0, 1), run(0x7FFF_FFFF_FFFF, u32::MAX as u64 + 1)],
        ];
        for runs in lists {
            assert_eq!(decode(&encode(&runs)).unwrap(), runs);
        }
    }

    #[test]
    fn a_damaged_list_is_refused() {
        assert_eq!(decode(&[0x21, 0x18]), None);
        assert_eq!(decode(&[0x20, 0x18, 0x34, 0x56, 0]), None);
        assert_eq!(
            decode(&[0x11, 0x01, 0xFF, 0]),
            None,
            "a run before cluster 0"
        );
        assert_eq!(decode(&[]), None);
    }
}
