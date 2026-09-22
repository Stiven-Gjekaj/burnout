//! Every decision that the Linux device layer makes.
//!
//! This module makes no system call. It takes a [`SysfsSource`] and returns
//! drives, so its tests run on Linux, on macOS and on Windows alike.
//!
//! Nothing here may name a type from a crate that belongs to one host. Those
//! crates do not exist on the other two, and one such name would delete two
//! thirds of the tests without a word.

use std::collections::BTreeSet;

use burnout_core::{Bus, Connection, DriveId, DriveInfo, Error, Result};

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

/// One line of `/proc/self/mountinfo`, reduced to what matters here.
#[derive(Debug, PartialEq, Eq)]
pub struct MountSource {
    /// The major device number of the file system.
    pub major: u32,
    /// The minor device number.
    pub minor: u32,
    /// Where the file system is mounted.
    pub mount_point: String,
    /// What the file system was mounted from.
    pub source: String,
}

/// Read `/proc/self/mountinfo`.
///
/// A line looks like this, and the optional fields between the mount options
/// and the single dash are the reason the source cannot be counted from the
/// front:
///
/// ```text
/// 36 35 98:0 / /boot rw,noatime shared:1 - ext4 /dev/sda1 rw
/// ```
pub fn mountinfo_sources(text: &str) -> Vec<MountSource> {
    let mut out = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(separator) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        if fields.len() < separator + 3 || separator < 5 {
            continue;
        }
        let Some((major, minor)) = fields[2].split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) else {
            continue;
        };
        out.push(MountSource {
            major,
            minor,
            mount_point: fields[4].to_string(),
            source: fields[separator + 2].to_string(),
        });
    }
    out
}

/// Read the device names out of `/proc/swaps`.
///
/// A drive that holds the swap of the running system is not a drive that
/// anybody should overwrite.
pub fn swap_sources(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| name.starts_with("/dev/"))
        .map(|name| name.to_string())
        .collect()
}

/// The whole drive behind one device number.
///
/// `/sys/dev/block/<major>:<minor>` is a link into the device tree. The last
/// name in it is the device. A partition carries a `partition` file, and then
/// the drive is the name above it.
pub fn disk_for_dev(fs: &dyn SysfsSource, major: u32, minor: u32) -> Option<String> {
    let base = format!("/sys/dev/block/{major}:{minor}");
    let target = fs.read_link(&base).ok()?;
    let parts: Vec<&str> = target
        .split('/')
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .collect();
    if attribute(fs, &format!("{base}/partition")).is_some() {
        // A partition. The drive is the directory above it.
        parts
            .len()
            .checked_sub(2)
            .and_then(|i| parts.get(i))
            .map(|s| s.to_string())
    } else {
        parts.last().map(|s| s.to_string())
    }
}

/// The real drives under one block device.
///
/// A device mapper or a RAID device is not a drive. It sits on other devices,
/// and `slaves` names them. Root on an encrypted volume or on a mirror has to
/// mark every drive under it, or Burnout offers to erase the disk that the
/// running system starts from.
pub fn base_disks(fs: &dyn SysfsSource, name: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut seen = BTreeSet::new();
    walk_slaves(fs, name, &mut found, &mut seen);
    found
}

fn walk_slaves(
    fs: &dyn SysfsSource,
    name: &str,
    found: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
) {
    // A cycle here would loop forever. The kernel makes none, and a test
    // double can.
    if !seen.insert(name.to_string()) {
        return;
    }
    if is_drive_name(name) {
        found.insert(name.to_string());
        return;
    }
    if let Ok(slaves) = fs.read_dir(&format!("{BLOCK}/{name}/slaves")) {
        for slave in slaves {
            // A slave may be a partition, such as sda3 under dm-0. The drive
            // is the name with the partition number taken off.
            let disk = partition_to_disk(fs, &slave);
            walk_slaves(fs, &disk, found, seen);
        }
    }
}

/// The drive that a partition belongs to.
///
/// `/sys/class/block/<partition>` links into the device tree, and the drive
/// is the directory above the partition.
fn partition_to_disk(fs: &dyn SysfsSource, name: &str) -> String {
    let base = format!("/sys/class/block/{name}");
    if attribute(fs, &format!("{base}/partition")).is_none() {
        return name.to_string();
    }
    let Ok(target) = fs.read_link(&base) else {
        return name.to_string();
    };
    let parts: Vec<&str> = target
        .split('/')
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .collect();
    parts
        .len()
        .checked_sub(2)
        .and_then(|i| parts.get(i))
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Every drive that carries the backing file of a mounted loop device.
///
/// A live USB is why this exists. Fedora, Ubuntu and Arch start the same way:
/// the medium holds one large file, a loop device presents that file as a
/// block device, and the root of the running system is an overlay on top of
/// it. Nothing in that chain names the drive, so the rules below mark nothing
/// and Burnout offers to erase the drive it is running from.
///
/// The kernel names the file in `loop/backing_file`, and it names it as the
/// loop was set up. In an initramfs that is a path relative to a mount point
/// that no longer exists under that name, so the file is looked for under
/// every mount point.
///
/// **A drive is marked only when one mount holds that file.** The kernel says
/// what the file was called and not which file was opened, and two mounts can
/// hold the same name: a Fedora live USB and a Fedora disc both carry
/// `LiveOS/squashfs.img`. A disc counts as a holder too, although Burnout
/// never writes one. Both cases are measured: one machine started from the
/// stick with the disc in the drive, and another started from the disc with
/// the stick in a port, and the two mount tables differ only in the names of
/// the mount points. A mark is a refusal that no option overrides, so a guess
/// would take a drive away from its owner for good. When it is not certain
/// this marks nothing, the list says the system disk is unknown, and the
/// write command answers that with the stronger prompt.
pub fn loop_backed_disks(fs: &dyn SysfsSource, mountinfo: &str) -> BTreeSet<String> {
    let mounts = mountinfo_sources(mountinfo);
    let mut out = BTreeSet::new();
    for mount in &mounts {
        let Some(name) = mount.source.strip_prefix("/dev/") else {
            continue;
        };
        if !name.starts_with("loop") {
            continue;
        }
        let Some(backing) = attribute(fs, &format!("{BLOCK}/{name}/loop/backing_file")) else {
            continue;
        };
        let (directory, file) = match backing.rsplit_once('/') {
            Some((directory, file)) => (directory, file),
            None => ("", backing.as_str()),
        };
        if file.is_empty() {
            continue;
        }

        // Each device that holds the file, a disc included, and the drives
        // under those devices.
        let mut holders = BTreeSet::new();
        let mut drives = BTreeSet::new();
        for candidate in &mounts {
            if candidate.source.starts_with("/dev/loop") {
                // A loop device does not carry its own backing file.
                continue;
            }
            let base = candidate.mount_point.trim_end_matches('/');
            let tail = directory.trim_start_matches('/');
            let at = if tail.is_empty() {
                format!("{base}/")
            } else {
                format!("{base}/{tail}")
            };
            let Ok(names) = fs.read_dir(&at) else {
                continue;
            };
            if !names.iter().any(|n| n == file) {
                continue;
            }
            let disk = if candidate.major == 0 {
                candidate
                    .source
                    .strip_prefix("/dev/")
                    .map(|n| n.to_string())
            } else {
                disk_for_dev(fs, candidate.major, candidate.minor)
            };
            if let Some(disk) = disk {
                let disk = partition_to_disk(fs, &disk);
                drives.extend(base_disks(fs, &disk));
                holders.insert(disk);
            }
        }
        // One holder is an answer. Two are a guess, and a mark is a refusal
        // that no option overrides. A disc that is the one holder marks
        // nothing, because it is not a drive.
        if holders.len() == 1 {
            out.extend(drives);
        }
    }
    out
}

/// Every drive that the running system needs.
///
/// This is deliberately a superset: the root, the boot directory, the EFI
/// partition and the swap. Refusing one drive too many costs somebody a
/// second of reading. Refusing one too few costs them the machine.
pub fn system_disks(fs: &dyn SysfsSource, mountinfo: &str, swaps: &str) -> BTreeSet<String> {
    const MARKED: [&str; 3] = ["/", "/boot", "/boot/efi"];
    let mut out = BTreeSet::new();

    for mount in mountinfo_sources(mountinfo) {
        if !MARKED.contains(&mount.mount_point.as_str()) {
            continue;
        }
        let name = if mount.major == 0 {
            // The file system has no device number of its own. btrfs and
            // overlay do this. The source path is the only route left.
            mount.source.strip_prefix("/dev/").map(|n| n.to_string())
        } else {
            disk_for_dev(fs, mount.major, mount.minor)
        };
        if let Some(name) = name {
            let name = partition_to_disk(fs, &name);
            out.extend(base_disks(fs, &name));
        }
    }

    for swap in swap_sources(swaps) {
        if let Some(name) = swap.strip_prefix("/dev/") {
            let name = partition_to_disk(fs, name);
            out.extend(base_disks(fs, &name));
        }
    }

    // A live USB reaches its root through a loop device, and the rules above
    // see none of that.
    out.extend(loop_backed_disks(fs, mountinfo));

    out
}

/// Every place that a volume of one drive is mounted.
///
/// The deepest path comes first. A file system mounted inside another one
/// holds the one under it busy, so the one under it cannot go first.
///
/// This takes the drives under a device mapper too. A partition of the target
/// that carries LVM or LUKS reaches the file system through another name, and
/// unmounting the name that `/dev/sdb2` carries would miss it.
pub fn mount_points_for_disk(fs: &dyn SysfsSource, mountinfo: &str, disk: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for mount in mountinfo_sources(mountinfo) {
        let name = if mount.major == 0 {
            // The file system has no device number of its own. The source
            // path is the only route left.
            mount.source.strip_prefix("/dev/").map(|n| n.to_string())
        } else {
            disk_for_dev(fs, mount.major, mount.minor)
        };
        let Some(name) = name else {
            continue;
        };
        let name = partition_to_disk(fs, &name);
        if base_disks(fs, &name).contains(disk) && !out.contains(&mount.mount_point) {
            out.push(mount.mount_point);
        }
    }
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    out
}

/// Build one drive from what `sysfs` says about it.
pub fn drive(fs: &dyn SysfsSource, name: &str, system: &BTreeSet<String>) -> Option<DriveInfo> {
    if !is_listable(fs, name) {
        return None;
    }
    let size = size_bytes(&attribute(fs, &format!("{BLOCK}/{name}/size"))?).ok()?;
    let (vendor, model, serial) = identity(fs, name);
    let (bus, connection) = bus(fs, name);

    let mut info = DriveInfo::new(
        DriveId::new(name),
        format!("/dev/{name}"),
        display_name(vendor.as_deref(), model.as_deref(), name),
    );
    info.vendor = vendor;
    info.model = model;
    info.serial = serial;
    info.size_bytes = size;
    info.logical_sector_size = logical_sector_size(fs, name);
    info.physical_sector_size = physical_sector_size(fs, name);
    info.bus = bus;
    info.connection = connection;
    info.removable_media = removable_media(fs, name);
    info.system = system.contains(name);
    Some(info)
}

/// Every drive that this host reports.
///
/// One drive that the kernel describes badly does not fail the whole list.
/// A list that stops at an empty card reader is a list that nobody can use.
pub fn list_drives(fs: &dyn SysfsSource, mountinfo: &str, swaps: &str) -> Result<Vec<DriveInfo>> {
    let names = fs.read_dir(BLOCK).map_err(|e| Error::Host {
        source: BLOCK.to_string(),
        detail: e.to_string(),
    })?;
    let system = system_disks(fs, mountinfo, swaps);
    Ok(names
        .iter()
        .filter_map(|name| drive(fs, name, &system))
        .collect())
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

    /// A whole machine: one SATA system disk and one USB stick.
    fn machine() -> MapSysfs {
        let sata = "../../devices/pci0000:00/0000:00:17.0/ata3/host2/target2:0:0/2:0:0:0/block/sda";
        let usb = "../../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb";
        with_sata_disk(MapSysfs::new())
            .dir("/sys/block", &["sda", "sdb", "loop0", "sr0"])
            .link("/sys/block/sda", sata)
            .file("/sys/block/sda/size", "1953525168")
            .file("/sys/block/sda/removable", "0")
            .file("/sys/block/sda/queue/logical_block_size", "512")
            .file("/sys/block/sda/device/vendor", "ATA     ")
            .file("/sys/block/sda/device/model", "Samsung SSD 870 ")
            .link("/sys/block/sdb", usb)
            .file("/sys/block/sdb/size", "62521344")
            .file("/sys/block/sdb/removable", "1")
            .file("/sys/block/sdb/queue/logical_block_size", "512")
            .file("/sys/block/sdb/device/vendor", "SanDisk ")
            .file("/sys/block/sdb/device/model", "Ultra           ")
            .file("/sys/block/loop0/size", "204800")
            .file("/sys/block/sr0/size", "2097151")
    }

    #[test]
    fn a_machine_lists_its_drives_and_nothing_else() {
        let drives = list_drives(&machine(), ROOT_ON_SDA2, "").unwrap();
        let names: Vec<&str> = drives.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(names, ["sda", "sdb"], "loop0 and sr0 are not drives");
    }

    #[test]
    fn the_system_drive_is_marked_and_the_usb_stick_is_not() {
        let drives = list_drives(&machine(), ROOT_ON_SDA2, "").unwrap();
        let sda = drives.iter().find(|d| d.id.as_str() == "sda").unwrap();
        let sdb = drives.iter().find(|d| d.id.as_str() == "sdb").unwrap();
        assert!(sda.system);
        assert!(!sdb.system);
    }

    #[test]
    fn a_drive_carries_the_size_in_bytes_and_not_in_sectors() {
        let drives = list_drives(&machine(), ROOT_ON_SDA2, "").unwrap();
        let sda = drives.iter().find(|d| d.id.as_str() == "sda").unwrap();
        assert_eq!(sda.size_bytes, 1_000_204_886_016);
        assert_eq!(sda.node, "/dev/sda");
    }

    #[test]
    fn the_usb_stick_reads_as_removable_and_the_system_disk_does_not() {
        let drives = list_drives(&machine(), ROOT_ON_SDA2, "").unwrap();
        let sda = drives.iter().find(|d| d.id.as_str() == "sda").unwrap();
        let sdb = drives.iter().find(|d| d.id.as_str() == "sdb").unwrap();
        assert!(sdb.removable());
        assert!(!sda.removable());
        assert_eq!(sdb.bus, Bus::Usb);
        assert_eq!(sda.bus, Bus::Sata);
        assert_eq!(sdb.name, "SanDisk Ultra");
    }

    #[test]
    fn a_host_with_no_block_directory_is_an_error_and_not_an_empty_list() {
        // An empty list would say that the machine has no drive, which is a
        // different thing from not being able to look.
        let fs = MapSysfs::new();
        assert!(list_drives(&fs, "", "").is_err());
    }

    // Lines captured by hand from a running machine.
    /// One USB stick with two mounted partitions, and the system disk beside
    /// it. Captured by hand from a Fedora machine.
    const TWO_DRIVES_MOUNTED: &str = "\
36 35 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
41 36 8:1 / /boot rw,relatime shared:2 - ext4 /dev/sda1 rw
58 36 8:17 / /run/media/me/BOOT rw,nosuid,nodev shared:3 - vfat /dev/sdb1 rw
59 36 8:18 / /run/media/me/INSTALL rw,nosuid,nodev shared:4 - exfat /dev/sdb2 rw
60 59 8:18 /nested /run/media/me/INSTALL/deep rw shared:5 - exfat /dev/sdb2 rw";

    fn with_usb_stick(fs: MapSysfs) -> MapSysfs {
        let tree = "../../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb";
        fs.link("/sys/dev/block/8:16", tree)
            .link("/sys/dev/block/8:17", &format!("{tree}/sdb1"))
            .file("/sys/dev/block/8:17/partition", "1")
            .link("/sys/dev/block/8:18", &format!("{tree}/sdb2"))
            .file("/sys/dev/block/8:18/partition", "2")
            .link("/sys/class/block/sdb1", &format!("{tree}/sdb1"))
            .file("/sys/class/block/sdb1/partition", "1")
            .link("/sys/class/block/sdb2", &format!("{tree}/sdb2"))
            .file("/sys/class/block/sdb2/partition", "2")
    }

    #[test]
    fn the_mount_points_of_one_drive_leave_the_other_drive_alone() {
        // Unmounting a path that belongs to the system disk would take the
        // running machine apart, so this is the test that matters most here.
        let fs = with_usb_stick(with_sata_disk(MapSysfs::new()));
        let points = mount_points_for_disk(&fs, TWO_DRIVES_MOUNTED, "sdb");
        assert!(points.contains(&"/run/media/me/BOOT".to_string()));
        assert!(points.contains(&"/run/media/me/INSTALL".to_string()));
        assert!(!points.contains(&"/".to_string()));
        assert!(!points.contains(&"/boot".to_string()));
    }

    #[test]
    fn the_deepest_mount_point_comes_first() {
        // A file system mounted inside another holds the one under it busy.
        let fs = with_usb_stick(with_sata_disk(MapSysfs::new()));
        let points = mount_points_for_disk(&fs, TWO_DRIVES_MOUNTED, "sdb");
        let deep = points
            .iter()
            .position(|p| p == "/run/media/me/INSTALL/deep")
            .expect("the nested mount is there");
        let shallow = points
            .iter()
            .position(|p| p == "/run/media/me/INSTALL")
            .expect("the mount under it is there");
        assert!(deep < shallow);
    }

    #[test]
    fn a_drive_with_nothing_mounted_gives_no_mount_point() {
        let fs = with_usb_stick(with_sata_disk(MapSysfs::new()));
        assert!(mount_points_for_disk(&fs, ROOT_ON_SDA2, "sdb").is_empty());
    }

    #[test]
    fn a_volume_reached_through_a_device_mapper_still_names_its_drive() {
        // LUKS on the stick. The mount says dm-0, and dm-0 says sdb2 through
        // its slaves. A search that only matched the name of the partition
        // would unmount nothing and then fail to open the drive.
        let fs = with_usb_stick(with_sata_disk(MapSysfs::new()))
            .link("/sys/dev/block/253:0", "../../devices/virtual/block/dm-0")
            .dir("/sys/block/dm-0/slaves", &["sdb2"])
            .link("/sys/class/block/sdb2", "../../devices/x/block/sdb/sdb2")
            .file("/sys/class/block/sdb2/partition", "2");
        let text = "70 36 253:0 / /mnt/secret rw shared:9 - ext4 /dev/mapper/secret rw";
        assert_eq!(
            mount_points_for_disk(&fs, text, "sdb"),
            ["/mnt/secret".to_string()]
        );
    }

    /// A Fedora live USB, captured by hand from a running one. The root is an
    /// overlay with no device of its own, the medium is sdb1, and the only
    /// link between them is the loop device in the middle.
    const LIVE_USB: &str = "\
81 1 0:37 / / rw,relatime shared:1 - overlay LiveOS_rootfs rw,lowerdir=/run/rootfsbase,upperdir=/run/overlayfs
53 51 8:17 / /run/initramfs/live ro,relatime shared:17 - iso9660 /dev/sdb1 ro,nojoliet
54 51 7:0 / /run/rootfsbase ro,relatime shared:18 - erofs /dev/loop0 ro,seclabel
1116 51 8:1 / /run/media/liveuser/XFER rw,nosuid,nodev shared:1042 - vfat /dev/sda1 rw";

    fn live_machine() -> MapSysfs {
        let tree = "../../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb";
        MapSysfs::new()
            // The kernel names the file as the loop was set up, which was
            // inside the initramfs, so the path is relative to a mount point
            // that no longer carries that name.
            .file("/sys/block/loop0/loop/backing_file", "/LiveOS/squashfs.img")
            .dir("/run/initramfs/live/LiveOS", &["squashfs.img", "osmin.img"])
            .link("/sys/dev/block/8:17", &format!("{tree}/sdb1"))
            .file("/sys/dev/block/8:17/partition", "1")
            .link("/sys/class/block/sdb1", &format!("{tree}/sdb1"))
            .file("/sys/class/block/sdb1/partition", "1")
    }

    #[test]
    fn the_drive_that_a_live_system_started_from_is_a_system_disk() {
        // Without this, Burnout offers to erase the USB stick it is running
        // from. Nothing in the mount table names that stick: the root is an
        // overlay with no device, and the medium is reached through a loop.
        let found = system_disks(&live_machine(), LIVE_USB, "");
        assert!(
            found.contains("sdb"),
            "the live medium is marked: {found:?}"
        );
        assert!(!found.contains("sda"), "the other stick is not: {found:?}");
    }

    #[test]
    fn a_loop_whose_file_is_missing_marks_nothing_and_does_not_fail() {
        // The file can be deleted after the loop is set up. That is not a
        // reason to stop, and it is not a reason to mark a drive at random.
        let fs = MapSysfs::new().file("/sys/block/loop0/loop/backing_file", "/gone/image.img");
        assert!(loop_backed_disks(&fs, LIVE_USB).is_empty());
    }

    #[test]
    fn a_loop_with_no_backing_file_at_all_marks_nothing() {
        assert!(loop_backed_disks(&MapSysfs::new(), LIVE_USB).is_empty());
    }

    #[test]
    fn a_stick_and_a_disc_that_hold_the_same_name_mark_neither() {
        // Started from the stick, with the disc in the drive. Both carry
        // LiveOS/squashfs.img, and the kernel says only what the file was
        // called. The next test is the same machine started from the disc,
        // and its mount table has the same shape, so neither can be marked.
        let fs = live_machine()
            .dir(
                "/run/media/liveuser/Fedora-WS-Live-44/LiveOS",
                &["squashfs.img"],
            )
            .link("/sys/dev/block/11:0", "../../devices/x/block/sr0");
        let text = "\
81 1 0:37 / / rw,relatime shared:1 - overlay LiveOS_rootfs rw,lowerdir=/run/rootfsbase
53 51 8:17 / /run/initramfs/live ro,relatime shared:17 - iso9660 /dev/sdb1 ro
54 51 7:0 / /run/rootfsbase ro,relatime shared:18 - erofs /dev/loop0 ro
1143 51 11:0 / /run/media/liveuser/Fedora-WS-Live-44 ro,nosuid shared:1068 - iso9660 /dev/sr0 ro";
        let found = loop_backed_disks(&fs, text);
        assert!(
            found.is_empty(),
            "a guess between a stick and a disc: {found:?}"
        );
    }

    #[test]
    fn a_stick_that_holds_a_copy_of_the_disc_the_system_started_from_is_not_marked() {
        // Measured: the machine started from the disc, and the stick in the
        // port held the same image, which the desktop mounted. Burnout
        // refused the stick as the system disk, and no option overrides that
        // refusal. The disc is a holder too, so this is a guess, and a guess
        // marks nothing.
        let tree = "../../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb";
        let fs = MapSysfs::new()
            .file("/sys/block/loop0/loop/backing_file", "/LiveOS/squashfs.img")
            .dir("/run/initramfs/live/LiveOS", &["squashfs.img", "osmin.img"])
            .dir(
                "/run/media/liveuser/Fedora-WS-Live-44/LiveOS",
                &["squashfs.img", "osmin.img"],
            )
            .link("/sys/dev/block/11:0", "../../devices/x/block/sr0")
            .link("/sys/dev/block/8:17", &format!("{tree}/sdb1"))
            .file("/sys/dev/block/8:17/partition", "1")
            .link("/sys/class/block/sdb1", &format!("{tree}/sdb1"))
            .file("/sys/class/block/sdb1/partition", "1");
        let text = "\
81 1 0:37 / / rw,relatime shared:1 - overlay LiveOS_rootfs rw,lowerdir=/run/rootfsbase
53 51 11:0 / /run/initramfs/live ro,relatime shared:17 - iso9660 /dev/sr0 ro
54 51 7:0 / /run/rootfsbase ro,relatime shared:18 - erofs /dev/loop0 ro
1143 51 8:17 / /run/media/liveuser/Fedora-WS-Live-44 ro,nosuid shared:1068 - iso9660 /dev/sdb1 ro";
        let found = loop_backed_disks(&fs, text);
        assert!(
            found.is_empty(),
            "the stick is not the system disk: {found:?}"
        );
    }

    #[test]
    fn a_disc_that_is_the_only_holder_marks_nothing() {
        // Started from the disc with no copy anywhere else. The disc is the
        // one holder and it is not a drive, so no drive is marked.
        let fs = MapSysfs::new()
            .file("/sys/block/loop0/loop/backing_file", "/LiveOS/squashfs.img")
            .dir("/run/initramfs/live/LiveOS", &["squashfs.img"])
            .link("/sys/dev/block/11:0", "../../devices/x/block/sr0");
        let text = "\
81 1 0:37 / / rw,relatime shared:1 - overlay LiveOS_rootfs rw,lowerdir=/run/rootfsbase
53 51 11:0 / /run/initramfs/live ro,relatime shared:17 - iso9660 /dev/sr0 ro
54 51 7:0 / /run/rootfsbase ro,relatime shared:18 - erofs /dev/loop0 ro";
        assert!(loop_backed_disks(&fs, text).is_empty());
    }

    #[test]
    fn two_drives_that_hold_the_same_name_mark_neither() {
        // Two sticks of the same live image. Only one of them backs the loop
        // and the kernel does not say which. A mark is a refusal that no
        // option overrides, so a guess would take a drive away from its owner
        // for good. The list then says the system disk is unknown, and the
        // write command answers that with the stronger prompt.
        let other = "../../devices/pci0000:00/0000:00:14.0/usb2/2-2/block/sdc";
        let fs = live_machine()
            .dir("/run/media/liveuser/COPY/LiveOS", &["squashfs.img"])
            .link("/sys/dev/block/8:33", &format!("{other}/sdc1"))
            .file("/sys/dev/block/8:33/partition", "1")
            .link("/sys/class/block/sdc1", &format!("{other}/sdc1"))
            .file("/sys/class/block/sdc1/partition", "1");
        let text = "\
81 1 0:37 / / rw,relatime shared:1 - overlay LiveOS_rootfs rw,lowerdir=/run/rootfsbase
53 51 8:17 / /run/initramfs/live ro,relatime shared:17 - iso9660 /dev/sdb1 ro
54 51 7:0 / /run/rootfsbase ro,relatime shared:18 - erofs /dev/loop0 ro
99 51 8:33 / /run/media/liveuser/COPY rw,nosuid shared:99 - vfat /dev/sdc1 rw";
        let found = loop_backed_disks(&fs, text);
        assert!(found.is_empty(), "a guess between two drives: {found:?}");
    }

    #[test]
    fn a_machine_with_no_loop_device_is_unchanged() {
        let fs = with_sata_disk(MapSysfs::new());
        assert!(loop_backed_disks(&fs, ROOT_ON_SDA2).is_empty());
    }

    const ROOT_ON_SDA2: &str =
        "36 35 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw,errors=remount-ro";
    const EFI_ON_SDA1: &str = "40 36 8:1 / /boot/efi rw,relatime shared:5 - vfat /dev/sda1 rw";

    /// A drive, its partition, and the links that tie them together.
    fn with_sata_disk(fs: MapSysfs) -> MapSysfs {
        let tree = "../../devices/pci0000:00/0000:00:17.0/ata3/host2/target2:0:0/2:0:0:0/block/sda";
        fs.link("/sys/dev/block/8:0", tree)
            .link("/sys/dev/block/8:1", &format!("{tree}/sda1"))
            .file("/sys/dev/block/8:1/partition", "1")
            .link("/sys/dev/block/8:2", &format!("{tree}/sda2"))
            .file("/sys/dev/block/8:2/partition", "2")
            .link("/sys/class/block/sda1", &format!("{tree}/sda1"))
            .file("/sys/class/block/sda1/partition", "1")
            .link("/sys/class/block/sda2", &format!("{tree}/sda2"))
            .file("/sys/class/block/sda2/partition", "2")
            .link("/sys/class/block/sda3", &format!("{tree}/sda3"))
            .file("/sys/class/block/sda3/partition", "3")
    }

    #[test]
    fn a_mountinfo_line_gives_the_device_number_and_the_source() {
        let got = mountinfo_sources(ROOT_ON_SDA2);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].major, 8);
        assert_eq!(got[0].minor, 2);
        assert_eq!(got[0].mount_point, "/");
        assert_eq!(got[0].source, "/dev/sda2");
    }

    #[test]
    fn the_optional_fields_do_not_move_the_source() {
        // The count of fields between the options and the dash changes from
        // line to line, so the source is found from the dash and not from
        // the front.
        let none = "36 35 8:2 / / rw - ext4 /dev/sda2 rw";
        let two = "36 35 8:2 / / rw shared:1 master:2 - ext4 /dev/sda2 rw";
        assert_eq!(mountinfo_sources(none)[0].source, "/dev/sda2");
        assert_eq!(mountinfo_sources(two)[0].source, "/dev/sda2");
    }

    #[test]
    fn a_line_with_no_dash_is_skipped_and_does_not_panic() {
        assert!(mountinfo_sources("36 35 8:2 / / rw ext4 /dev/sda2").is_empty());
        assert!(mountinfo_sources("").is_empty());
        assert!(mountinfo_sources("nonsense").is_empty());
    }

    #[test]
    fn the_swap_file_gives_its_devices_and_skips_the_header() {
        let swaps = "Filename\t\t\t\tType\t\tSize\tUsed\tPriority\n\
                     /dev/sda3                               partition\t8388604\t0\t-2\n";
        assert_eq!(swap_sources(swaps), ["/dev/sda3"]);
    }

    #[test]
    fn a_swap_in_a_file_is_not_a_device() {
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n/swapfile\tfile\t2097148\t0\t-2\n";
        assert!(swap_sources(swaps).is_empty());
    }

    #[test]
    fn a_partition_resolves_to_the_drive_above_it() {
        let fs = with_sata_disk(MapSysfs::new());
        assert_eq!(disk_for_dev(&fs, 8, 2).unwrap(), "sda");
        assert_eq!(disk_for_dev(&fs, 8, 0).unwrap(), "sda");
    }

    #[test]
    fn the_root_partition_marks_its_drive() {
        let fs = with_sata_disk(MapSysfs::new());
        assert_eq!(system_disks(&fs, ROOT_ON_SDA2, ""), set(&["sda"]));
    }

    #[test]
    fn the_efi_partition_marks_its_drive_as_well() {
        let fs = with_sata_disk(MapSysfs::new());
        assert_eq!(system_disks(&fs, EFI_ON_SDA1, ""), set(&["sda"]));
    }

    #[test]
    fn a_drive_that_holds_the_swap_is_a_system_drive() {
        let fs = with_sata_disk(MapSysfs::new());
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n/dev/sda3\tpartition\t8388604\t0\t-2\n";
        assert_eq!(system_disks(&fs, "", swaps), set(&["sda"]));
    }

    #[test]
    fn root_on_an_encrypted_volume_marks_the_drive_under_it() {
        // /dev/mapper/root is dm-0, which sits on sda3. Marking dm-0 alone
        // would leave sda open to be erased.
        let fs = with_sata_disk(MapSysfs::new())
            .link("/sys/dev/block/254:0", "../../devices/virtual/block/dm-0")
            .dir("/sys/block/dm-0/slaves", &["sda3"]);
        let line = "36 35 254:0 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw";
        assert_eq!(system_disks(&fs, line, ""), set(&["sda"]));
    }

    #[test]
    fn root_on_a_mirror_marks_every_drive_in_it() {
        let tree_b =
            "../../devices/pci0000:00/0000:00:17.0/ata4/host3/target3:0:0/3:0:0:0/block/sdb";
        let fs = with_sata_disk(MapSysfs::new())
            .link("/sys/dev/block/9:0", "../../devices/virtual/block/md0")
            .dir("/sys/block/md0/slaves", &["sda1", "sdb1"])
            .link("/sys/class/block/sdb1", &format!("{tree_b}/sdb1"))
            .file("/sys/class/block/sdb1/partition", "1");
        let line = "36 35 9:0 / / rw,relatime shared:1 - ext4 /dev/md0 rw";
        assert_eq!(system_disks(&fs, line, ""), set(&["sda", "sdb"]));
    }

    #[test]
    fn root_on_btrfs_is_found_through_the_source_path() {
        // btrfs gives the file system a device number of its own, with a
        // major of zero, so /sys/dev/block holds nothing for it.
        let fs = with_sata_disk(MapSysfs::new());
        let line = "36 35 0:35 / / rw,relatime shared:1 - btrfs /dev/sda2 rw,subvol=/@";
        assert_eq!(system_disks(&fs, line, ""), set(&["sda"]));
    }

    #[test]
    fn root_on_an_overlay_marks_no_drive_and_does_not_fail() {
        // This is a container. The honest answer is that there is no system
        // drive here. The command that writes a drive then refuses every
        // fixed disk, because it cannot prove which one is safe.
        let fs = with_sata_disk(MapSysfs::new());
        let line = "36 35 0:42 / / rw,relatime - overlay overlay rw,lowerdir=/a,upperdir=/b";
        assert!(system_disks(&fs, line, "").is_empty());
    }

    #[test]
    fn a_ring_of_slaves_does_not_loop_forever() {
        // The kernel makes no such ring. A test double can, and a walk with
        // no guard would never return.
        let fs = MapSysfs::new()
            .dir("/sys/block/dm-0/slaves", &["dm-1"])
            .dir("/sys/block/dm-1/slaves", &["dm-0"]);
        assert!(base_disks(&fs, "dm-0").is_empty());
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
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
