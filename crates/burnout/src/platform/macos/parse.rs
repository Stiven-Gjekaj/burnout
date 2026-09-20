//! Every decision that the macOS device layer makes.
//!
//! This module makes no system call. It takes a [`RegistrySnapshot`] and
//! returns drives, so its tests run on macOS, on Linux and on Windows alike.
//!
//! Nothing here may name a type from `core-foundation` or `io-kit-sys`.

use std::collections::BTreeSet;

use burnout_core::{Bus, Connection, DriveId, DriveInfo};

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

/// Find the media node that carries one device number.
pub fn media_for_dev(snapshot: &RegistrySnapshot, major: i64, minor: i64) -> Option<usize> {
    snapshot.media.iter().copied().find(|index| {
        let node_major = snapshot.get(*index, "BSD Major").and_then(Value::as_int);
        let node_minor = snapshot.get(*index, "BSD Minor").and_then(Value::as_int);
        node_major == Some(major) && node_minor == Some(minor)
    })
}

/// This is what resolves APFS. A system volume sits inside a container, and
/// the container sits on one or more physical stores. Walking up from the
/// volume reaches the real drives, and a container built from two stores
/// reaches both of them.
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

fn is_whole(snapshot: &RegistrySnapshot, index: usize) -> bool {
    let node = match snapshot.nodes.get(index) {
        Some(n) => n,
        None => return false,
    };
    node.class == "IOMedia" && node.get("Whole").and_then(Value::as_bool) == Some(true)
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
