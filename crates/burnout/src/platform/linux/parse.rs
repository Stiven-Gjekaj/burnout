//! Every decision that the Linux device layer makes.
//!
//! This module makes no system call. It takes a [`SysfsSource`] and returns
//! drives, so its tests run on Linux, on macOS and on Windows alike.
//!
//! Nothing here may name a type from a crate that belongs to one host. Those
//! crates do not exist on the other two, and one such name would delete two
//! thirds of the tests without a word.

use burnout_core::{Error, Result};

use super::source::{attribute, SysfsSource};

/// Where the block devices live.
pub const BLOCK: &str = "/sys/block";

/// The unit of the `size` attribute, in bytes.
///
/// The kernel reports `size` in units of 512 bytes on every drive, whatever
/// the sector size of that drive is. This is the trap of the whole Linux
/// backend. Multiplying `size` by `logical_block_size` is right on every
/// drive of 512 byte sectors, and it gives eight times the real size on a
/// drive of 4096 byte sectors.
pub const SIZE_UNIT: u64 = 512;

/// The size of a drive in bytes, from the `size` attribute.
pub fn size_bytes(size_attribute: &str) -> Result<u64> {
    let sectors: u64 = size_attribute.trim().parse().map_err(|_| Error::Host {
        source: "sysfs size".to_string(),
        detail: format!("{size_attribute:?} is not a count of sectors"),
    })?;
    sectors.checked_mul(SIZE_UNIT).ok_or_else(|| Error::Host {
        source: "sysfs size".to_string(),
        detail: format!("{sectors} sectors of {SIZE_UNIT} bytes does not fit in a u64"),
    })
}

/// The logical sector size of a drive, which is 512 when the kernel is quiet.
pub fn logical_sector_size(fs: &dyn SysfsSource, name: &str) -> u32 {
    attribute(fs, &format!("{BLOCK}/{name}/queue/logical_block_size"))
        .and_then(|t| t.parse().ok())
        .unwrap_or(512)
}

/// The physical sector size of a drive, when the kernel reports one.
pub fn physical_sector_size(fs: &dyn SysfsSource, name: &str) -> Option<u32> {
    attribute(fs, &format!("{BLOCK}/{name}/queue/physical_block_size")).and_then(|t| t.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::super::source::fake::MapSysfs;
    use super::*;

    #[test]
    fn a_size_is_a_count_of_512_byte_sectors() {
        // A one terabyte drive of 512 byte sectors.
        assert_eq!(size_bytes("1953525168").unwrap(), 1_000_204_886_016);
    }

    #[test]
    fn a_4096_byte_drive_reports_its_size_in_512_byte_units_as_well() {
        // This is the trap. The drive holds 64,023,257,088 bytes. Anybody
        // who multiplies this count by the 4096 byte sector size gets eight
        // times that, and every 512 byte drive they own still looks right.
        let sectors = "125045424";
        assert_eq!(size_bytes(sectors).unwrap(), 64_023_257_088);
        assert_ne!(size_bytes(sectors).unwrap(), 125_045_424 * 4096);
    }

    #[test]
    fn a_size_with_the_newline_still_on_it_reads() {
        assert_eq!(size_bytes("2048\n").unwrap(), 1_048_576);
    }

    #[test]
    fn a_drive_of_no_size_is_zero_and_not_an_error() {
        // An empty card reader reports zero. It is not a broken drive.
        assert_eq!(size_bytes("0").unwrap(), 0);
    }

    #[test]
    fn a_size_that_is_not_a_number_is_an_error() {
        assert!(size_bytes("many").is_err());
        assert!(size_bytes("").is_err());
        assert!(size_bytes("-1").is_err());
    }

    #[test]
    fn a_size_that_does_not_fit_is_an_error_and_not_a_wrong_number() {
        assert!(size_bytes(&u64::MAX.to_string()).is_err());
    }

    #[test]
    fn a_sector_size_comes_from_the_queue_directory() {
        let fs = MapSysfs::new().file("/sys/block/sda/queue/logical_block_size", "4096\n");
        assert_eq!(logical_sector_size(&fs, "sda"), 4096);
    }

    #[test]
    fn a_quiet_kernel_means_512_byte_sectors() {
        let fs = MapSysfs::new();
        assert_eq!(logical_sector_size(&fs, "sda"), 512);
    }

    #[test]
    fn a_physical_sector_size_is_absent_when_the_kernel_says_nothing() {
        let fs = MapSysfs::new().file("/sys/block/sda/queue/physical_block_size", "4096");
        assert_eq!(physical_sector_size(&fs, "sda"), Some(4096));
        assert_eq!(physical_sector_size(&fs, "sdb"), None);
    }

    #[test]
    fn a_drive_can_hold_4096_byte_sectors_on_a_512_byte_interface() {
        // The common 512e drive: the medium uses 4096 and the interface
        // still speaks 512.
        let fs = MapSysfs::new()
            .file("/sys/block/sda/queue/logical_block_size", "512")
            .file("/sys/block/sda/queue/physical_block_size", "4096");
        assert_eq!(logical_sector_size(&fs, "sda"), 512);
        assert_eq!(physical_sector_size(&fs, "sda"), Some(4096));
    }
}
