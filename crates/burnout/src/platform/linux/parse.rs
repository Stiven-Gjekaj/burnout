//! Every decision that the Linux device layer makes.
//!
//! This module makes no system call. It takes a [`SysfsSource`] and returns
//! drives, so its tests run on Linux, on macOS and on Windows alike.
//!
//! Nothing here may name a type from a crate that belongs to one host. Those
//! crates do not exist on the other two, and one such name would delete two
//! thirds of the tests without a word.

use burnout_core::{Bus, Connection, Error, Result};

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

/// The bus that carries a drive, and where the drive sits.
///
/// The kernel writes the whole device path into the symbolic link at
/// `/sys/block/<name>`, so the ancestors of the drive are in that one string.
///
/// The order of these checks matters. A USB drive hangs off a SCSI host, so
/// its path holds both `usb2` and `host6`. The nearer bus wins, and USB is
/// the one a person unplugs.
pub fn bus_from_device_path(link_target: &str) -> (Bus, Connection) {
    let segments: Vec<&str> = link_target.split('/').collect();
    let has = |prefix: &str| segments.iter().any(|s| s.starts_with(prefix));

    if has("usb") {
        (Bus::Usb, Connection::External)
    } else if has("mmc_host") {
        (Bus::Mmc, Connection::External)
    } else if has("fw-host") || has("firewire") {
        (Bus::FireWire, Connection::External)
    } else if has("nvme") {
        (Bus::Nvme, Connection::Internal)
    } else if has("virtio") {
        (Bus::Virtual, Connection::Internal)
    } else if has("ata") {
        (Bus::Sata, Connection::Internal)
    } else if has("host") || has("target") {
        (Bus::Scsi, Connection::Internal)
    } else {
        (Bus::Unknown, Connection::Unknown)
    }
}

/// The kernel names that `/sys/block` holds and that are not drives.
///
/// Each one is a block device, and none of them is a disk that a person
/// writes an image to.
const NOT_A_DRIVE: [&str; 8] = [
    "loop", // a file that the kernel presents as a device
    "ram",  // a disk in memory
    "zram", // the same, compressed
    "sr",   // an optical drive
    "dm-",  // device mapper, which sits on top of a real drive
    "md",   // software RAID, which sits on top of real drives
    "nbd",  // a network block device
    "fd",   // a floppy drive
];

/// Whether a kernel name belongs to a drive that Burnout lists.
///
/// This looks at the name only. A name that passes still has to carry a size
/// and stay visible, which [`is_listable`] checks.
pub fn is_drive_name(kernel_name: &str) -> bool {
    !kernel_name.is_empty() && !NOT_A_DRIVE.iter().any(|p| kernel_name.starts_with(p))
}

/// Whether Burnout lists this drive.
///
/// A hidden device is one that the kernel keeps for its own use, such as the
/// single paths under an NVMe drive that has more than one. A device of no
/// size is a card reader with no card in it, and there is nothing to write.
pub fn is_listable(fs: &dyn SysfsSource, kernel_name: &str) -> bool {
    if !is_drive_name(kernel_name) {
        return false;
    }
    if attribute(fs, &format!("{BLOCK}/{kernel_name}/hidden")).as_deref() == Some("1") {
        return false;
    }
    match attribute(fs, &format!("{BLOCK}/{kernel_name}/size")) {
        Some(size) => size_bytes(&size).map(|b| b > 0).unwrap_or(false),
        None => false,
    }
}

/// The bus and the place of one drive, read through the source.
///
/// The kernel writes the device path into the symbolic link at
/// `/sys/block/<name>`. A drive with no such link tells us nothing, and an
/// unknown bus is better than a guessed one.
pub fn bus(fs: &dyn SysfsSource, name: &str) -> (Bus, Connection) {
    match fs.read_link(&format!("{BLOCK}/{name}")) {
        Ok(target) => bus_from_device_path(&target),
        Err(_) => (Bus::Unknown, Connection::Unknown),
    }
}

/// The name that the `DRIVE` column prints.
///
/// SCSI pads the vendor to eight characters and the model to sixteen, so both
/// arrive with trailing spaces. An NVMe drive has a model and no vendor. An
/// SD card has neither, and then the kernel name is the only name there is.
pub fn display_name(vendor: Option<&str>, model: Option<&str>, kernel_name: &str) -> String {
    let vendor = vendor.map(str::trim).filter(|t| !t.is_empty());
    let model = model.map(str::trim).filter(|t| !t.is_empty());
    match (vendor, model) {
        (Some(v), Some(m)) if m.starts_with(v) => m.to_string(),
        (Some(v), Some(m)) => format!("{v} {m}"),
        (None, Some(m)) => m.to_string(),
        (Some(v), None) => v.to_string(),
        (None, None) => kernel_name.to_string(),
    }
}

/// Whether the kernel says the medium comes out of the drive.
pub fn removable_media(fs: &dyn SysfsSource, name: &str) -> bool {
    attribute(fs, &format!("{BLOCK}/{name}/removable")).as_deref() == Some("1")
}

/// The vendor, the model and the serial number, when the drive carries them.
pub fn identity(
    fs: &dyn SysfsSource,
    name: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let at = |leaf: &str| attribute(fs, &format!("{BLOCK}/{name}/device/{leaf}"));
    // An SD card has no vendor and no model. It has a name instead.
    let model = at("model").or_else(|| at("name"));
    (at("vendor"), model, at("serial"))
}

#[cfg(test)]
mod tests {
    use super::super::source::fake::MapSysfs;
    use super::*;

    // Paths captured by hand from real machines.
    const USB_STICK: &str =
        "../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb";
    const SATA_SSD: &str =
        "../devices/pci0000:00/0000:00:17.0/ata3/host2/target2:0:0/2:0:0:0/block/sda";
    const NVME: &str = "../devices/pci0000:00/0000:00:1d.0/0000:3c:00.0/nvme/nvme0/nvme0n1";
    const SD_CARD: &str =
        "../devices/platform/soc/fe340000.mmc/mmc_host/mmc0/mmc0:59b4/block/mmcblk0";
    const VIRTIO: &str = "../devices/pci0000:00/0000:00:05.0/virtio2/block/vda";

    #[test]
    fn a_usb_drive_beats_the_scsi_host_it_hangs_off() {
        // The path holds host6 and target6:0:0 as well. The nearer bus wins,
        // because USB is the one that a person unplugs.
        assert_eq!(
            bus_from_device_path(USB_STICK),
            (Bus::Usb, Connection::External)
        );
    }

    #[test]
    fn each_bus_comes_out_of_its_own_path() {
        assert_eq!(
            bus_from_device_path(SATA_SSD),
            (Bus::Sata, Connection::Internal)
        );
        assert_eq!(
            bus_from_device_path(NVME),
            (Bus::Nvme, Connection::Internal)
        );
        assert_eq!(
            bus_from_device_path(SD_CARD),
            (Bus::Mmc, Connection::External)
        );
        assert_eq!(
            bus_from_device_path(VIRTIO),
            (Bus::Virtual, Connection::Internal)
        );
    }

    #[test]
    fn a_path_that_names_no_bus_is_unknown_and_not_a_guess() {
        assert_eq!(
            bus_from_device_path("../devices/platform/something/block/xyz0"),
            (Bus::Unknown, Connection::Unknown)
        );
    }

    #[test]
    fn a_real_drive_name_passes() {
        for name in ["sda", "sdb", "nvme0n1", "mmcblk0", "vda", "hda"] {
            assert!(is_drive_name(name), "{name} should be a drive");
        }
    }

    #[test]
    fn the_block_devices_that_are_not_drives_are_refused() {
        for name in [
            "loop0", "ram0", "zram0", "sr0", "dm-0", "md0", "nbd0", "fd0",
        ] {
            assert!(!is_drive_name(name), "{name} should not be a drive");
        }
    }

    #[test]
    fn an_empty_name_is_not_a_drive() {
        assert!(!is_drive_name(""));
    }

    #[test]
    fn a_drive_with_a_size_is_listed() {
        let fs = MapSysfs::new().file("/sys/block/sda/size", "1953525168");
        assert!(is_listable(&fs, "sda"));
    }

    #[test]
    fn a_card_reader_with_no_card_is_not_listed() {
        // It reports a size of zero. There is nothing to write to.
        let fs = MapSysfs::new().file("/sys/block/sdc/size", "0");
        assert!(!is_listable(&fs, "sdc"));
    }

    #[test]
    fn a_hidden_device_is_not_listed() {
        // An NVMe drive with more than one path hides the single paths.
        let fs = MapSysfs::new()
            .file("/sys/block/nvme0c0n1/size", "1953525168")
            .file("/sys/block/nvme0c0n1/hidden", "1");
        assert!(!is_listable(&fs, "nvme0c0n1"));
    }

    #[test]
    fn a_device_that_reports_no_size_at_all_is_not_listed() {
        let fs = MapSysfs::new();
        assert!(!is_listable(&fs, "sda"));
    }

    #[test]
    fn a_loop_device_with_a_size_is_still_not_listed() {
        // The name decides first, so a mounted image never reaches the list.
        let fs = MapSysfs::new().file("/sys/block/loop0/size", "204800");
        assert!(!is_listable(&fs, "loop0"));
    }

    #[test]
    fn the_bus_comes_through_the_link_that_the_kernel_writes() {
        let fs = MapSysfs::new().link("/sys/block/sdb", USB_STICK);
        assert_eq!(bus(&fs, "sdb"), (Bus::Usb, Connection::External));
    }

    #[test]
    fn a_drive_with_no_link_has_an_unknown_bus_and_not_a_guessed_one() {
        let fs = MapSysfs::new();
        assert_eq!(bus(&fs, "sdb"), (Bus::Unknown, Connection::Unknown));
    }

    #[test]
    fn a_scsi_name_loses_the_padding_that_scsi_adds() {
        // SCSI pads the vendor to eight and the model to sixteen.
        assert_eq!(
            display_name(Some("SanDisk "), Some("Ultra           "), "sdb"),
            "SanDisk Ultra"
        );
    }

    #[test]
    fn an_nvme_drive_has_a_model_and_no_vendor() {
        assert_eq!(
            display_name(None, Some("Samsung SSD 980 PRO 1TB"), "nvme0n1"),
            "Samsung SSD 980 PRO 1TB"
        );
    }

    #[test]
    fn a_model_that_already_starts_with_the_vendor_does_not_say_it_twice() {
        assert_eq!(
            display_name(Some("Samsung"), Some("Samsung SSD 870"), "sda"),
            "Samsung SSD 870"
        );
    }

    #[test]
    fn a_drive_with_no_name_at_all_falls_back_to_the_kernel_name() {
        assert_eq!(display_name(None, None, "mmcblk0"), "mmcblk0");
        assert_eq!(display_name(Some("   "), Some(""), "sdc"), "sdc");
    }

    #[test]
    fn a_vendor_with_no_model_still_gives_a_name() {
        assert_eq!(display_name(Some("Generic"), None, "sdd"), "Generic");
    }

    #[test]
    fn the_removable_flag_is_the_digit_one_and_nothing_else() {
        let yes = MapSysfs::new().file("/sys/block/sdb/removable", "1\n");
        let no = MapSysfs::new().file("/sys/block/sda/removable", "0\n");
        assert!(removable_media(&yes, "sdb"));
        assert!(!removable_media(&no, "sda"));
        assert!(!removable_media(&MapSysfs::new(), "sdc"));
    }

    #[test]
    fn an_sd_card_takes_its_model_from_the_name_attribute() {
        // An SD card carries no vendor and no model. It carries a name.
        let fs = MapSysfs::new().file("/sys/block/mmcblk0/device/name", "SD32G\n");
        let (vendor, model, serial) = identity(&fs, "mmcblk0");
        assert_eq!(vendor, None);
        assert_eq!(model.as_deref(), Some("SD32G"));
        assert_eq!(serial, None);
    }

    #[test]
    fn a_scsi_drive_carries_all_three_parts_of_its_identity() {
        let fs = MapSysfs::new()
            .file("/sys/block/sdb/device/vendor", "SanDisk \n")
            .file("/sys/block/sdb/device/model", "Ultra           \n")
            .file("/sys/block/sdb/device/serial", "4C530001234\n");
        let (vendor, model, serial) = identity(&fs, "sdb");
        assert_eq!(vendor.as_deref(), Some("SanDisk"));
        assert_eq!(model.as_deref(), Some("Ultra"));
        assert_eq!(serial.as_deref(), Some("4C530001234"));
    }

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
