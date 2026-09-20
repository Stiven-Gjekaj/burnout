//! Sector arithmetic.
//!
//! A raw device refuses a read or a write that its sector size does not
//! divide. Burnout supports a logical sector of 512 bytes and one of 4096
//! bytes, and nothing else.

use crate::{Error, Result};

/// The sector sizes that Burnout writes.
pub const SECTOR_SIZES: [u32; 2] = [512, 4096];

/// Refuse a sector size that no drive in scope reports.
///
/// A host that answers something else is a host that this code does not
/// understand, and a guess there writes a drive wrongly.
pub fn check_sector_size(sector_size: u32) -> Result<u32> {
    if SECTOR_SIZES.contains(&sector_size) {
        Ok(sector_size)
    } else {
        Err(Error::SectorSize { got: sector_size })
    }
}

/// Say whether a length fills whole sectors.
pub fn is_aligned(length: u64, sector_size: u32) -> bool {
    sector_size != 0 && length % u64::from(sector_size) == 0
}

/// Refuse a length that does not fill whole sectors.
pub fn check_aligned(length: u64, sector_size: u32) -> Result<()> {
    if is_aligned(length, sector_size) {
        Ok(())
    } else {
        Err(Error::Unaligned {
            length,
            sector_size,
        })
    }
}

/// Grow a length to the next whole sector.
///
/// A source image rarely ends on a sector boundary. The last write to the
/// device pads to one, and these are the bytes to write.
pub fn round_up(length: u64, sector_size: u32) -> u64 {
    let sector = u64::from(sector_size);
    if sector == 0 {
        return length;
    }
    length.div_ceil(sector) * sector
}

/// How many whole sectors a length fills, rounded up.
pub fn sector_count(length: u64, sector_size: u32) -> u64 {
    let sector = u64::from(sector_size);
    if sector == 0 {
        return 0;
    }
    length.div_ceil(sector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_sizes_that_drives_report_are_accepted() {
        assert_eq!(check_sector_size(512).unwrap(), 512);
        assert_eq!(check_sector_size(4096).unwrap(), 4096);
    }

    #[test]
    fn any_other_size_is_refused() {
        for size in [0, 1, 256, 1024, 2048, 8192] {
            assert!(check_sector_size(size).is_err(), "{size} was accepted");
        }
    }

    #[test]
    fn a_length_of_whole_sectors_is_aligned() {
        assert!(is_aligned(0, 512));
        assert!(is_aligned(512, 512));
        assert!(is_aligned(4096, 512));
        assert!(is_aligned(4096, 4096));
    }

    #[test]
    fn a_length_that_a_512_byte_drive_takes_can_still_fail_on_a_4096_byte_one() {
        // This is the case that a test on one drive never finds. The same
        // image ends on a sector of one drive and in the middle of a sector
        // of the other.
        assert!(is_aligned(2560, 512));
        assert!(!is_aligned(2560, 4096));
    }

    #[test]
    fn an_unaligned_length_is_refused_and_the_error_carries_both_numbers() {
        let e = check_aligned(1000, 512).unwrap_err();
        let text = e.to_string();
        assert!(text.contains("1000"));
        assert!(text.contains("512"));
    }

    #[test]
    fn rounding_up_leaves_a_whole_sector_alone() {
        assert_eq!(round_up(4096, 4096), 4096);
        assert_eq!(round_up(0, 4096), 0);
    }

    #[test]
    fn rounding_up_fills_the_last_sector() {
        assert_eq!(round_up(1, 512), 512);
        assert_eq!(round_up(513, 512), 1024);
        assert_eq!(round_up(4097, 4096), 8192);
    }

    #[test]
    fn a_sector_count_rounds_up() {
        assert_eq!(sector_count(0, 512), 0);
        assert_eq!(sector_count(1, 512), 1);
        assert_eq!(sector_count(512, 512), 1);
        assert_eq!(sector_count(513, 512), 2);
    }

    #[test]
    fn a_large_length_does_not_overflow() {
        // An eight terabyte drive, which holds more sectors than a u32 counts.
        let whole = 8_001_563_222_016_u64;
        assert!(
            is_aligned(whole, 4096),
            "the next check means nothing unless this is whole"
        );
        assert_eq!(sector_count(whole, 512), 15_628_053_168);

        // One byte past a whole sector, so the rounding does real work.
        let ragged = whole + 1;
        assert_eq!(round_up(ragged, 4096), whole + 4096);
        assert_eq!(sector_count(ragged, 4096), 1_953_506_647);
    }
}
