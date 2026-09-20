//! Every decision that the macOS device layer makes.
//!
//! This module makes no system call. It takes a [`RegistrySnapshot`] and
//! returns drives, so its tests run on macOS, on Linux and on Windows alike.
//!
//! Nothing here may name a type from `core-foundation` or `io-kit-sys`.

use std::collections::BTreeSet;

use burnout_core::{Bus, Connection, DriveId, DriveInfo, Result};

use super::model::{RegistrySnapshot, Value};

/// The bus, from the `Physical Interconnect` that IOKit reports.
pub fn bus_from_interconnect(text: &str) -> Bus {
    match text {
        "USB" => Bus::Usb,
        "SATA" | "Serial ATA" => Bus::Sata,
        "ATA" => Bus::Ata,
        "SCSI Parallel Interface" => Bus::Scsi,
        "SAS" | "Serial Attached SCSI" => Bus::Sas,
        "PCI-Express" => Bus::PciExpress,
        "Apple Fabric" => Bus::AppleFabric,
        "Secure Digital" => Bus::Sd,
        "FireWire" => Bus::FireWire,
        "Thunderbolt" => Bus::Thunderbolt,
        "Virtual Interface" => Bus::Virtual,
        _ => Bus::Unknown,
    }
}

/// Where the drive sits, from `Physical Interconnect Location`.
pub fn connection_from_location(text: &str) -> Connection {
    match text {
        "Internal" => Connection::Internal,
        "External" => Connection::External,
        // A slot that a person reaches counts as external, because the drive
        // comes off the desk.
        "Internal/External" => Connection::External,
        _ => Connection::Unknown,
    }
}

/// Split a device number the way Darwin builds it.
pub fn split_dev(dev: u64) -> (i64, i64) {
    let major = ((dev >> 24) & 0xff) as i64;
    let minor = (dev & 0xffffff) as i64;
    (major, minor)
}

/// Find the media node that carries one device number.
pub fn media_for_dev(snapshot: &RegistrySnapshot, major: i64, minor: i64) -> Option<usize> {
    snapshot.media.iter().copied().find(|index| {
        let node_major = snapshot.get(*index, "BSD Major").and_then(Value::as_int);
        let node_minor = snapshot.get(*index, "BSD Minor").and_then(Value::as_int);
        node_major == Some(major) && node_minor == Some(minor)
    })
}

/// Every whole drive at or above one media node.
///
/// This is what resolves APFS. The chain from a system volume runs
/// `disk3s1s1`, `AppleAPFSVolume`, `AppleAPFSMedia disk3`,
/// `AppleAPFSContainerScheme`, `IOMedia disk0s2`, `IOGUIDPartitionScheme`,
/// `IOMedia disk0`. Walking it reaches the real drive, and a container built
/// on two stores reaches both of them.
///
/// `diskutil` would answer this in one call, and `diskutil` is the tool that
/// the rules of this project forbid.
pub fn whole_disks_above(snapshot: &RegistrySnapshot, index: usize) -> Vec<usize> {
    let mut found = Vec::new();
    if is_whole(snapshot, index) {
        found.push(index);
    }
    let mut here = index;
    while let Some(parent) = snapshot.nodes.get(here).and_then(|n| n.parent) {
        if is_whole(snapshot, parent) && !found.contains(&parent) {
            found.push(parent);
        }
        here = parent;
    }
    found
}

/// Whether a node is a whole device rather than a partition.
///
/// The class is matched by its ending and not by its whole name.
/// `IOServiceMatching("IOMedia")` returns the subclasses too, and the
/// synthesised container of an APFS volume arrives as `AppleAPFSMedia`. A
/// check for the exact name drops it, and then the drive that the running
/// system starts from is not marked.
fn is_whole(snapshot: &RegistrySnapshot, index: usize) -> bool {
    let node = match snapshot.nodes.get(index) {
        Some(n) => n,
        None => return false,
    };
    node.class.ends_with("Media") && node.get("Whole").and_then(Value::as_bool) == Some(true)
}

/// Build one drive from one whole media node.
pub fn drive_from_media(
    snapshot: &RegistrySnapshot,
    index: usize,
    system: &BTreeSet<String>,
) -> Option<DriveInfo> {
    if !is_whole(snapshot, index) {
        return None;
    }
    let name = snapshot.get(index, "BSD Name").and_then(Value::as_text)?;
    let size = snapshot.get(index, "Size").and_then(Value::as_int)? as u64;
    if size == 0 {
        // A card reader with no card. There is nothing to write to.
        return None;
    }

    let characteristics = snapshot.find_dict(index, "Device Characteristics");
    let text = |key: &str| {
        characteristics
            .and_then(|d| d.get(key))
            .and_then(Value::as_text)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    let vendor = text("Vendor Name");
    let model = text("Product Name");
    let serial = text("Serial Number");

    let protocol = snapshot.find_dict(index, "Protocol Characteristics");
    let protocol_text = |key: &str| {
        protocol
            .and_then(|d| d.get(key))
            .and_then(Value::as_text)
            .unwrap_or("")
    };

    let display = match (vendor.as_deref(), model.as_deref()) {
        (Some(v), Some(m)) if m.starts_with(v) => m.to_string(),
        (Some(v), Some(m)) => format!("{v} {m}"),
        (None, Some(m)) => m.to_string(),
        (Some(v), None) => v.to_string(),
        (None, None) => name.to_string(),
    };

    let mut info = DriveInfo::new(
        DriveId::new(name),
        // The raw node, and never /dev/diskN. The raw node skips the buffer
        // cache and is about ten times faster.
        format!("/dev/r{name}"),
        display,
    );
    info.vendor = vendor;
    info.model = model;
    info.serial = serial;
    info.size_bytes = size;
    info.logical_sector_size = snapshot
        .get(index, "Preferred Block Size")
        .and_then(Value::as_int)
        .unwrap_or(512) as u32;
    info.bus = bus_from_interconnect(protocol_text("Physical Interconnect"));
    info.connection = connection_from_location(protocol_text("Physical Interconnect Location"));
    info.removable_media = snapshot
        .get(index, "Removable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || snapshot
            .get(index, "Ejectable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    info.system = system.contains(name);
    Some(info)
}

/// Every whole drive in one snapshot.
pub fn list_drives(snapshot: &RegistrySnapshot, root_dev: Option<u64>) -> Result<Vec<DriveInfo>> {
    let system = system_disks(snapshot, root_dev);
    Ok(snapshot
        .media
        .iter()
        .filter_map(|index| drive_from_media(snapshot, *index, &system))
        .collect())
}

/// The drives that the running system starts from.
pub fn system_disks(snapshot: &RegistrySnapshot, root_dev: Option<u64>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(dev) = root_dev else {
        return out;
    };
    let (major, minor) = split_dev(dev);
    let Some(media) = media_for_dev(snapshot, major, minor) else {
        return out;
    };
    for whole in whole_disks_above(snapshot, media) {
        if let Some(name) = snapshot.get(whole, "BSD Name").and_then(Value::as_text) {
            out.insert(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::model::Node;
    use super::*;
    use std::collections::BTreeMap;

    fn characteristics(vendor: &str, product: &str) -> Value {
        Value::Dict(BTreeMap::from([
            ("Vendor Name".to_string(), Value::Text(vendor.to_string())),
            ("Product Name".to_string(), Value::Text(product.to_string())),
        ]))
    }

    fn protocol(interconnect: &str, location: &str) -> Value {
        Value::Dict(BTreeMap::from([
            (
                "Physical Interconnect".to_string(),
                Value::Text(interconnect.to_string()),
            ),
            (
                "Physical Interconnect Location".to_string(),
                Value::Text(location.to_string()),
            ),
        ]))
    }

    /// One whole drive under a controller that carries the names.
    fn one_drive(name: &str, size: i64, interconnect: &str, location: &str) -> RegistrySnapshot {
        let controller = Node::new("IOBlockStorageDriver")
            .with(
                "Device Characteristics",
                characteristics("SanDisk", "Ultra"),
            )
            .with("Protocol Characteristics", protocol(interconnect, location));
        let media = Node::new("IOMedia")
            .with("BSD Name", Value::Text(name.to_string()))
            .with("Whole", Value::Bool(true))
            .with("Size", Value::Int(size))
            .with("Preferred Block Size", Value::Int(512))
            .under(0);
        RegistrySnapshot {
            nodes: vec![controller, media],
            media: vec![1],
        }
    }

    /// An Apple silicon Mac: disk0 holds an APFS container that the system
    /// volume lives in.
    fn apple_silicon() -> RegistrySnapshot {
        let controller = Node::new("IOBlockStorageDriver")
            .with(
                "Device Characteristics",
                characteristics("Apple", "APPLE SSD AP1024Z"),
            )
            .with(
                "Protocol Characteristics",
                protocol("Apple Fabric", "Internal"),
            );
        let disk0 = Node::new("IOMedia")
            .with("BSD Name", Value::Text("disk0".to_string()))
            .with("Whole", Value::Bool(true))
            .with("Size", Value::Int(994_662_584_320))
            .with("Preferred Block Size", Value::Int(4096))
            .under(0);
        let scheme = Node::new("IOGUIDPartitionScheme").under(1);
        let disk0s2 = Node::new("IOMedia")
            .with("BSD Name", Value::Text("disk0s2".to_string()))
            .with("Whole", Value::Bool(false))
            .with("Size", Value::Int(994_000_000_000))
            .under(2);
        let container = Node::new("AppleAPFSContainerScheme").under(3);
        let disk3 = Node::new("AppleAPFSMedia")
            .with("BSD Name", Value::Text("disk3".to_string()))
            .with("Whole", Value::Bool(true))
            .with("Size", Value::Int(994_000_000_000))
            .under(4);
        let volume = Node::new("AppleAPFSVolume")
            .with("BSD Name", Value::Text("disk3s1s1".to_string()))
            .with("BSD Major", Value::Int(1))
            .with("BSD Minor", Value::Int(14))
            .with("Whole", Value::Bool(false))
            .under(5);
        RegistrySnapshot {
            nodes: vec![controller, disk0, scheme, disk0s2, container, disk3, volume],
            media: vec![1, 3, 5, 6],
        }
    }

    #[test]
    fn a_darwin_device_number_splits_into_a_major_and_a_minor() {
        // major 1, minor 14
        assert_eq!(split_dev((1 << 24) | 14), (1, 14));
        assert_eq!(split_dev(0), (0, 0));
    }

    #[test]
    fn the_system_volume_of_an_apple_silicon_mac_reaches_the_real_drive() {
        // The volume is disk3s1s1, inside container disk3, on store disk0s2,
        // on drive disk0. Marking disk3 alone would leave disk0 open.
        let t = apple_silicon();
        let got = system_disks(&t, Some((1 << 24) | 14));
        assert!(
            got.contains("disk0"),
            "the physical drive must be marked, got {got:?}"
        );
    }

    #[test]
    fn the_apfs_container_is_marked_as_well_as_the_drive() {
        let t = apple_silicon();
        let got = system_disks(&t, Some((1 << 24) | 14));
        assert!(got.contains("disk3"));
    }

    #[test]
    fn a_root_device_that_matches_no_media_marks_nothing() {
        let t = apple_silicon();
        assert!(system_disks(&t, Some((99 << 24) | 99)).is_empty());
    }

    #[test]
    fn no_root_device_at_all_marks_nothing_and_does_not_fail() {
        let t = apple_silicon();
        assert!(system_disks(&t, None).is_empty());
    }

    #[test]
    fn a_partition_walks_up_to_exactly_one_whole_drive() {
        let t = apple_silicon();
        let wholes = whole_disks_above(&t, 3);
        let names: Vec<&str> = wholes
            .iter()
            .filter_map(|i| t.get(*i, "BSD Name").and_then(Value::as_text))
            .collect();
        assert_eq!(names, ["disk0"]);
    }

    #[test]
    fn the_list_marks_the_system_drive_and_leaves_the_others_alone() {
        let t = apple_silicon();
        let drives = list_drives(&t, Some((1 << 24) | 14)).unwrap();
        let disk0 = drives.iter().find(|d| d.id.as_str() == "disk0").unwrap();
        assert!(disk0.system);
        assert_eq!(disk0.logical_sector_size, 4096);
    }

    #[test]
    fn every_interconnect_that_a_mac_reports_maps_to_a_bus() {
        assert_eq!(bus_from_interconnect("USB"), Bus::Usb);
        assert_eq!(bus_from_interconnect("PCI-Express"), Bus::PciExpress);
        assert_eq!(bus_from_interconnect("Apple Fabric"), Bus::AppleFabric);
        assert_eq!(bus_from_interconnect("Secure Digital"), Bus::Sd);
        assert_eq!(bus_from_interconnect("Virtual Interface"), Bus::Virtual);
    }

    #[test]
    fn an_interconnect_that_this_code_does_not_know_is_unknown_and_not_a_guess() {
        assert_eq!(bus_from_interconnect("Something New"), Bus::Unknown);
        assert_eq!(bus_from_interconnect(""), Bus::Unknown);
    }

    #[test]
    fn a_slot_that_a_person_reaches_counts_as_external() {
        assert_eq!(connection_from_location("Internal"), Connection::Internal);
        assert_eq!(connection_from_location("External"), Connection::External);
        assert_eq!(
            connection_from_location("Internal/External"),
            Connection::External
        );
        assert_eq!(connection_from_location("what"), Connection::Unknown);
    }

    #[test]
    fn a_whole_drive_reads_out_of_the_registry() {
        let t = one_drive("disk4", 32_010_928_128, "USB", "External");
        let d = drive_from_media(&t, 1, &BTreeSet::new()).unwrap();
        assert_eq!(d.id.as_str(), "disk4");
        assert_eq!(d.size_bytes, 32_010_928_128);
        assert_eq!(d.name, "SanDisk Ultra");
        assert_eq!(d.bus, Bus::Usb);
        assert_eq!(d.connection, Connection::External);
    }

    #[test]
    fn the_node_is_the_raw_one_because_the_other_goes_through_the_cache() {
        let t = one_drive("disk4", 1024, "USB", "External");
        let d = drive_from_media(&t, 1, &BTreeSet::new()).unwrap();
        assert_eq!(
            d.node, "/dev/rdisk4",
            "the buffered node is about ten times slower"
        );
    }

    #[test]
    fn a_usb_drive_with_no_removable_medium_still_reads_as_removable() {
        // A solid state drive in a USB case. macOS says External and says
        // the medium does not come out. A person unplugs it all the same.
        let t = one_drive("disk4", 1024, "USB", "External");
        let d = drive_from_media(&t, 1, &BTreeSet::new()).unwrap();
        assert!(!d.removable_media);
        assert!(d.removable());
    }

    #[test]
    fn a_partition_is_not_a_drive() {
        let mut t = one_drive("disk0", 1024, "Apple Fabric", "Internal");
        t.nodes.push(
            Node::new("IOMedia")
                .with("BSD Name", Value::Text("disk0s2".to_string()))
                .with("Whole", Value::Bool(false))
                .with("Size", Value::Int(512))
                .under(1),
        );
        t.media.push(2);
        assert!(drive_from_media(&t, 2, &BTreeSet::new()).is_none());
    }

    #[test]
    fn a_card_reader_with_no_card_is_not_listed() {
        let t = one_drive("disk3", 0, "Secure Digital", "Internal");
        assert!(drive_from_media(&t, 1, &BTreeSet::new()).is_none());
    }

    #[test]
    fn a_drive_with_no_name_at_all_falls_back_to_the_bsd_name() {
        let media = Node::new("IOMedia")
            .with("BSD Name", Value::Text("disk9".to_string()))
            .with("Whole", Value::Bool(true))
            .with("Size", Value::Int(1024));
        let t = RegistrySnapshot {
            nodes: vec![media],
            media: vec![0],
        };
        let d = drive_from_media(&t, 0, &BTreeSet::new()).unwrap();
        assert_eq!(d.name, "disk9");
        assert_eq!(d.bus, Bus::Unknown);
    }

    #[test]
    fn a_mounted_disk_image_is_listed_as_virtual_rather_than_hidden() {
        // A hidden target that a person can still write is worse than a
        // visible one.
        let t = one_drive("disk5", 12_884_901_888, "Virtual Interface", "Internal");
        let d = drive_from_media(&t, 1, &BTreeSet::new()).unwrap();
        assert_eq!(d.bus, Bus::Virtual);
    }
}
