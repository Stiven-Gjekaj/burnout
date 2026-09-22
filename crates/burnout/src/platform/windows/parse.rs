//! Every decision that the Windows device layer makes.
//!
//! The structures here are read at fixed offsets out of byte buffers, so this
//! module makes no system call and names no Windows type. Its tests run on
//! Linux and on macOS as well, which is where most of them run in practice.
//!
//! Nothing here may name a type from `windows-sys`. That crate belongs to one
//! host, and a name from it here would stop this file compiling on the other
//! two.
//!
//! Every reader below takes a length that the driver chose. A short buffer, a
//! length that lies and an offset that points past the end are all normal, so
//! each one gives an error and none of them reads outside the slice.

use std::collections::BTreeSet;

use burnout_core::{Bus, DriveId, DriveInfo, Error, Result};

use super::raw::{RawDisk, RawVolume};

/// `STORAGE_DEVICE_DESCRIPTOR`, as far as this code reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub revision: Option<String>,
    pub serial: Option<String>,
    pub removable_media: bool,
    pub bus_type: u8,
}

/// `DISK_GEOMETRY_EX`, as far as this code reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Geometry {
    pub bytes_per_sector: u32,
    pub disk_size: u64,
}

/// `STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Alignment {
    pub bytes_per_logical_sector: u32,
    pub bytes_per_physical_sector: u32,
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| short(offset, 4, bytes.len()))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    bytes
        .get(offset..offset + 8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| short(offset, 8, bytes.len()))
}

fn short(offset: usize, want: usize, have: usize) -> Error {
    Error::Host {
        source: "a Windows device answer".to_string(),
        detail: format!("{want} bytes at {offset} are not in a buffer of {have}"),
    }
}

/// Read a zero terminated string that an offset inside the buffer names.
///
/// An offset of zero means the field is absent, which is normal. An offset
/// past the end means the driver and the buffer disagree, and the field is
/// then absent as well rather than a read outside the slice.
fn string_at(bytes: &[u8], offset: u32) -> Option<String> {
    if offset == 0 {
        return None;
    }
    let start = offset as usize;
    let rest = bytes.get(start..)?;
    // A string that runs to the last byte with no terminator still ends.
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    let text = String::from_utf8_lossy(&rest[..end]).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Read `STORAGE_DEVICE_DESCRIPTOR`.
pub fn device_descriptor(bytes: &[u8]) -> Result<DeviceDescriptor> {
    // Version, Size, DeviceType, DeviceTypeModifier, RemovableMedia,
    // CommandQueueing, then four offsets, then BusType.
    const HEADER: usize = 36;
    if bytes.len() < HEADER {
        return Err(short(0, HEADER, bytes.len()));
    }
    let size = u32_at(bytes, 4)? as usize;
    // The driver says how much it wrote. Trust the smaller of the two, so a
    // length that lies cannot make a reader step outside the buffer.
    let usable = &bytes[..size.min(bytes.len()).max(HEADER)];

    Ok(DeviceDescriptor {
        vendor: string_at(usable, u32_at(usable, 12)?),
        product: string_at(usable, u32_at(usable, 16)?),
        revision: string_at(usable, u32_at(usable, 20)?),
        serial: string_at(usable, u32_at(usable, 24)?),
        removable_media: usable[10] != 0,
        bus_type: (u32_at(usable, 28)? & 0xff) as u8,
    })
}

/// Read `DISK_GEOMETRY_EX`.
pub fn disk_geometry_ex(bytes: &[u8]) -> Result<Geometry> {
    // Cylinders i64, MediaType u32, TracksPerCylinder u32, SectorsPerTrack
    // u32, BytesPerSector u32, DiskSize i64.
    Ok(Geometry {
        bytes_per_sector: u32_at(bytes, 20)?,
        disk_size: u64_at(bytes, 24)?,
    })
}

/// Read `STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR`.
pub fn access_alignment(bytes: &[u8]) -> Result<Alignment> {
    // Version u32, Size u32, BytesPerCacheLine u32, BytesOffsetForCacheAlign
    // u32, BytesPerLogicalSector u32, BytesPerPhysicalSector u32.
    Ok(Alignment {
        bytes_per_logical_sector: u32_at(bytes, 16)?,
        bytes_per_physical_sector: u32_at(bytes, 20)?,
    })
}

/// Read `VOLUME_DISK_EXTENTS` and give the disk numbers in it.
///
/// A volume that spans or mirrors holds more than one extent, and every disk
/// under it carries the running system.
pub fn disk_extents(bytes: &[u8]) -> Result<Vec<u32>> {
    let count = u32_at(bytes, 0)? as usize;
    // Each extent is DiskNumber u32, padding, StartingOffset i64,
    // ExtentLength i64, and the array starts after the count and its padding.
    const FIRST: usize = 8;
    const EACH: usize = 24;
    let room = bytes.len().saturating_sub(FIRST) / EACH;
    if count > room {
        return Err(Error::Host {
            source: "VOLUME_DISK_EXTENTS".to_string(),
            detail: format!("{count} extents do not fit in {} bytes", bytes.len()),
        });
    }
    (0..count)
        .map(|index| u32_at(bytes, FIRST + index * EACH))
        .collect()
}

/// The volumes that sit on one disk, ready to open.
///
/// A drive is not written while a file system holds its volumes, so the write
/// locks every volume of the target first. This says which ones they are.
///
/// The paths come back without the trailing backslash that
/// `FindFirstVolumeW` puts on the end. That one character is the whole
/// difference between a handle and an error.
pub fn volumes_on_disk(volumes: &[RawVolume], disk: u32) -> Vec<String> {
    let mut out = Vec::new();
    for volume in volumes {
        let Some(bytes) = volume.extents.as_deref() else {
            continue;
        };
        let Ok(disks) = disk_extents(bytes) else {
            continue;
        };
        if !disks.contains(&disk) {
            continue;
        }
        if let Some(path) = volume_path_to_open(&volume.guid_path) {
            out.push(path);
        }
    }
    out
}

/// Turn the name of a volume into the name that opens it.
///
/// `FindFirstVolumeW` gives a name that ends in a backslash, and `CreateFileW`
/// refuses that name and takes the one without it. A name that does not look
/// like a volume is dropped rather than trimmed into something else.
pub fn volume_path_to_open(guid_path: &str) -> Option<String> {
    let trimmed = guid_path.strip_suffix('\\').unwrap_or(guid_path);
    if !trimmed.starts_with(r"\\?\") || trimmed.len() <= r"\\?\".len() {
        return None;
    }
    Some(trimmed.to_string())
}

/// The bus, from `STORAGE_BUS_TYPE`.
pub fn bus_from_storage_bus_type(value: u8) -> Bus {
    match value {
        0x01 => Bus::Scsi,
        0x02 => Bus::Ata,
        0x03 => Bus::Ata,      // FLOPPY, through the ATA driver
        0x04 => Bus::Ata,      // 1394 on some drivers
        0x05 => Bus::Scsi,     // SSA
        0x06 => Bus::FireWire, // Fibre, reported for 1394 as well
        0x07 => Bus::Usb,
        0x08 => Bus::Raid,
        0x09 => Bus::Scsi, // iSCSI
        0x0A => Bus::Sas,
        0x0B => Bus::Sata,
        0x0C => Bus::Sd,
        0x0D => Bus::Mmc,
        0x0E => Bus::Virtual,
        0x0F => Bus::FireWire,
        0x10 => Bus::Virtual, // a storage space
        0x11 => Bus::Nvme,
        0x12 => Bus::Scsi, // SCM
        0x13 => Bus::Usb,  // UFS
        _ => Bus::Unknown,
    }
}

/// Whether the drive sits where a person can take it off the desk.
///
/// The bus decides. `SPDRP_REMOVAL_POLICY` looks like the right answer and is
/// not: NVMe supports hot plug, so a plain internal NVMe drive reports 2 or 3
/// as readily as a memory stick does. A build of this code that trusted the
/// policy called the system drive of a Windows runner removable, which is the
/// wrong answer in the dangerous direction.
///
/// So the policy is read only when the bus says nothing at all.
fn is_external(bus: Bus, policy: Option<u32>) -> bool {
    match bus {
        Bus::Usb | Bus::Sd | Bus::Mmc | Bus::FireWire | Bus::Thunderbolt => true,
        // 2 is orderly removal and 3 is surprise removal.
        Bus::Unknown => matches!(policy, Some(2) | Some(3)),
        _ => false,
    }
}

/// Build one drive from the answers about it.
pub fn drive_from_raw(raw: &RawDisk, system: &BTreeSet<u32>) -> Result<DriveInfo> {
    let descriptor = device_descriptor(&raw.device_descriptor)?;
    let geometry = disk_geometry_ex(&raw.geometry)?;
    let alignment = raw
        .alignment
        .as_deref()
        .and_then(|bytes| access_alignment(bytes).ok());

    let display = match (descriptor.vendor.as_deref(), descriptor.product.as_deref()) {
        (Some(v), Some(p)) if p.starts_with(v) => p.to_string(),
        (Some(v), Some(p)) => format!("{v} {p}"),
        (None, Some(p)) => p.to_string(),
        (Some(v), None) => v.to_string(),
        (None, None) => raw
            .friendly_name
            .clone()
            .unwrap_or_else(|| format!("PhysicalDrive{}", raw.device_number)),
    };

    let bus = bus_from_storage_bus_type(descriptor.bus_type);
    let mut info = DriveInfo::new(
        DriveId::new(raw.device_number.to_string()),
        format!(r"\\.\PhysicalDrive{}", raw.device_number),
        display,
    );
    info.vendor = descriptor.vendor;
    info.model = descriptor.product;
    info.serial = descriptor.serial;
    info.size_bytes = geometry.disk_size;
    info.logical_sector_size = alignment
        .map(|a| a.bytes_per_logical_sector)
        .filter(|s| *s > 0)
        .unwrap_or(geometry.bytes_per_sector);
    info.physical_sector_size = alignment
        .map(|a| a.bytes_per_physical_sector)
        .filter(|s| *s > 0);
    info.bus = bus;
    info.connection = if is_external(bus, raw.removal_policy) {
        burnout_core::Connection::External
    } else {
        burnout_core::Connection::Internal
    };
    info.removable_media = descriptor.removable_media;
    info.system = system.contains(&raw.device_number);
    Ok(info)
}

/// Every drive that the host answered about.
///
/// One disk that answers badly does not fail the whole list. A card reader
/// with no card refuses the geometry call, and a list that stops there is a
/// list that nobody can use.
pub fn list_drives(disks: &[RawDisk], system: &BTreeSet<u32>) -> Vec<DriveInfo> {
    disks
        .iter()
        .filter_map(|raw| drive_from_raw(raw, system).ok())
        .filter(|d| d.size_bytes > 0)
        .collect()
}

/// The space that UEFI keeps for the GPT entry array, whatever the number of
/// entries. No partition of a valid GPT starts inside it.
const GPT_ENTRY_ARRAY_BYTES: u64 = 16 * 1024;

/// Where a partition table can sit on a drive, as `(offset, length)` in bytes.
///
/// The first run holds the MBR, the GPT header and the GPT entries. The second
/// run holds the backup entries and the backup header at the end of the drive.
/// A valid GPT puts no partition in either run. So a write to them reaches no
/// partition, and Windows lets it through while the old table still holds.
///
/// A drive too small for the two runs is one run. A sector of zero gives none.
pub fn table_areas(length: u64, sector: u32) -> Vec<(u64, u64)> {
    let sector = u64::from(sector);
    if sector == 0 {
        return Vec::new();
    }
    let entries = GPT_ENTRY_ARRAY_BYTES.div_ceil(sector) * sector;
    let start = 2 * sector + entries;
    let end = entries + sector;
    if length <= start + end {
        return vec![(0, length)];
    }
    vec![(0, start), (length - end, end)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `STORAGE_DEVICE_DESCRIPTOR` the way a driver writes one.
    fn descriptor(vendor: &str, product: &str, removable: bool, bus: u8) -> Vec<u8> {
        const HEADER: usize = 36;
        let mut bytes = vec![0u8; HEADER];
        let push = |bytes: &mut Vec<u8>, text: &str| -> u32 {
            if text.is_empty() {
                return 0;
            }
            let at = bytes.len() as u32;
            bytes.extend_from_slice(text.as_bytes());
            bytes.push(0);
            at
        };
        let vendor_at = push(&mut bytes, vendor);
        let product_at = push(&mut bytes, product);

        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[10] = u8::from(removable);
        bytes[12..16].copy_from_slice(&vendor_at.to_le_bytes());
        bytes[16..20].copy_from_slice(&product_at.to_le_bytes());
        bytes[28..32].copy_from_slice(&(bus as u32).to_le_bytes());
        let size = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    fn geometry(sector: u32, size: u64) -> Vec<u8> {
        let mut bytes = vec![0u8; 32];
        bytes[20..24].copy_from_slice(&sector.to_le_bytes());
        bytes[24..32].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    fn extents(disks: &[u32]) -> Vec<u8> {
        let mut bytes = vec![0u8; 8 + disks.len() * 24];
        bytes[0..4].copy_from_slice(&(disks.len() as u32).to_le_bytes());
        for (index, disk) in disks.iter().enumerate() {
            let at = 8 + index * 24;
            bytes[at..at + 4].copy_from_slice(&disk.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn a_descriptor_gives_the_vendor_the_product_and_the_bus() {
        let d = device_descriptor(&descriptor("SanDisk", "Ultra", true, 0x07)).unwrap();
        assert_eq!(d.vendor.as_deref(), Some("SanDisk"));
        assert_eq!(d.product.as_deref(), Some("Ultra"));
        assert!(d.removable_media);
        assert_eq!(d.bus_type, 0x07);
    }

    #[test]
    fn a_field_with_an_offset_of_zero_is_absent_and_not_an_error() {
        let d = device_descriptor(&descriptor("", "Ultra", false, 0x0B)).unwrap();
        assert_eq!(d.vendor, None);
        assert_eq!(d.product.as_deref(), Some("Ultra"));
    }

    #[test]
    fn windows_pads_a_name_and_the_padding_comes_off() {
        let d = device_descriptor(&descriptor("SanDisk ", "Ultra           ", false, 7)).unwrap();
        assert_eq!(d.vendor.as_deref(), Some("SanDisk"));
        assert_eq!(d.product.as_deref(), Some("Ultra"));
    }

    #[test]
    fn a_buffer_shorter_than_the_header_is_an_error_and_not_a_panic() {
        for length in 0..36 {
            assert!(
                device_descriptor(&vec![0u8; length]).is_err(),
                "{length} bytes"
            );
        }
    }

    #[test]
    fn a_size_that_claims_more_than_the_buffer_holds_does_not_read_past_it() {
        let mut bytes = descriptor("SanDisk", "Ultra", false, 7);
        bytes[4..8].copy_from_slice(&9999u32.to_le_bytes());
        // The answer may be poor. It must not read outside the slice.
        let _ = device_descriptor(&bytes);
    }

    #[test]
    fn an_offset_past_the_end_leaves_the_field_absent() {
        let mut bytes = descriptor("SanDisk", "Ultra", false, 7);
        bytes[12..16].copy_from_slice(&9000u32.to_le_bytes());
        let d = device_descriptor(&bytes).unwrap();
        assert_eq!(d.vendor, None);
    }

    #[test]
    fn a_string_with_no_terminator_stops_at_the_end_of_the_buffer() {
        let mut bytes = vec![0u8; 36];
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"NoTerminator");
        let size = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        let d = device_descriptor(&bytes).unwrap();
        assert_eq!(d.vendor.as_deref(), Some("NoTerminator"));
    }

    #[test]
    fn a_geometry_gives_the_size_to_the_byte() {
        let g = disk_geometry_ex(&geometry(512, 1_000_204_886_016)).unwrap();
        assert_eq!(g.bytes_per_sector, 512);
        assert_eq!(g.disk_size, 1_000_204_886_016);
    }

    #[test]
    fn a_short_geometry_is_an_error() {
        assert!(disk_geometry_ex(&[0u8; 8]).is_err());
        assert!(disk_geometry_ex(&[]).is_err());
    }

    #[test]
    fn an_extent_list_gives_every_disk_under_the_volume() {
        // A mirrored system volume sits on two disks, and both carry the
        // running system.
        assert_eq!(disk_extents(&extents(&[0])).unwrap(), [0]);
        assert_eq!(disk_extents(&extents(&[0, 2])).unwrap(), [0, 2]);
    }

    fn volume(path: &str, disks: &[u32]) -> RawVolume {
        RawVolume {
            guid_path: path.to_string(),
            extents: Some(extents(disks)),
        }
    }

    const ON_DISK_0: &str = r"\\?\Volume{11111111-1111-1111-1111-111111111111}\";
    const ON_DISK_2: &str = r"\\?\Volume{22222222-2222-2222-2222-222222222222}\";

    #[test]
    fn only_the_volumes_of_the_named_disk_come_back() {
        // A lock on the wrong volume takes the system disk away from the
        // running machine, so this is the test that matters most here.
        let volumes = [volume(ON_DISK_0, &[0]), volume(ON_DISK_2, &[2])];
        let found = volumes_on_disk(&volumes, 2);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("22222222"));
        assert!(volumes_on_disk(&volumes, 1).is_empty());
    }

    #[test]
    fn a_volume_that_spans_two_disks_belongs_to_both() {
        let volumes = [volume(ON_DISK_0, &[0, 2])];
        assert_eq!(volumes_on_disk(&volumes, 0).len(), 1);
        assert_eq!(volumes_on_disk(&volumes, 2).len(), 1);
    }

    #[test]
    fn a_volume_that_answered_nothing_is_dropped_and_does_not_fail_the_list() {
        // An empty card reader answers nothing. Failing here would stop a
        // write to a drive that has nothing to do with that reader.
        let volumes = [
            RawVolume {
                guid_path: ON_DISK_0.to_string(),
                extents: None,
            },
            volume(ON_DISK_2, &[2]),
        ];
        assert_eq!(volumes_on_disk(&volumes, 2).len(), 1);
    }

    #[test]
    fn the_trailing_backslash_comes_off_the_volume_name() {
        // CreateFileW refuses the name that FindFirstVolumeW gives, and takes
        // the same name without its last character.
        assert_eq!(
            volume_path_to_open(ON_DISK_0).unwrap(),
            r"\\?\Volume{11111111-1111-1111-1111-111111111111}"
        );
    }

    #[test]
    fn a_name_that_is_not_a_volume_is_dropped_and_not_trimmed() {
        assert!(volume_path_to_open("").is_none());
        assert!(volume_path_to_open(r"\\?\").is_none());
        assert!(volume_path_to_open(r"C:\").is_none());
    }

    #[test]
    fn an_extent_count_larger_than_the_buffer_is_refused() {
        let mut bytes = extents(&[0]);
        bytes[0..4].copy_from_slice(&64u32.to_le_bytes());
        assert!(disk_extents(&bytes).is_err());
    }

    #[test]
    fn every_bus_that_windows_reports_maps_to_one_of_ours() {
        assert_eq!(bus_from_storage_bus_type(0x07), Bus::Usb);
        assert_eq!(bus_from_storage_bus_type(0x0B), Bus::Sata);
        assert_eq!(bus_from_storage_bus_type(0x11), Bus::Nvme);
        assert_eq!(bus_from_storage_bus_type(0x0C), Bus::Sd);
        assert_eq!(bus_from_storage_bus_type(0x0E), Bus::Virtual);
        assert_eq!(bus_from_storage_bus_type(0xFE), Bus::Unknown);
    }

    fn raw(
        number: u32,
        vendor: &str,
        product: &str,
        removable: bool,
        bus: u8,
        size: u64,
    ) -> RawDisk {
        RawDisk {
            interface_path: format!(r"\\?\disk{number}"),
            device_number: number,
            device_descriptor: descriptor(vendor, product, removable, bus),
            geometry: geometry(512, size),
            alignment: None,
            friendly_name: None,
            removal_policy: None,
        }
    }

    #[test]
    fn an_internal_nvme_drive_that_allows_hot_plug_is_not_removable() {
        // A Windows runner reports its own system drive this way: bus NVMe,
        // removal policy 3. An earlier build trusted the policy and called
        // the drive that the system starts from removable.
        let mut disk = raw(
            0,
            "Virtual_Disk",
            "NVME Premium",
            false,
            0x11,
            161_061_273_600,
        );
        disk.removal_policy = Some(3);
        let d = drive_from_raw(&disk, &BTreeSet::new()).unwrap();
        assert_eq!(d.connection, burnout_core::Connection::Internal);
        assert!(!d.removable(), "an internal NVMe drive is not removable");
    }

    #[test]
    fn a_usb_drive_is_removable_whatever_the_policy_says() {
        let mut disk = raw(1, "SanDisk", "Ultra", true, 0x07, 1024);
        disk.removal_policy = Some(1);
        assert!(drive_from_raw(&disk, &BTreeSet::new()).unwrap().removable());
    }

    #[test]
    fn the_policy_is_read_only_when_the_bus_says_nothing() {
        let mut unknown = raw(2, "Some", "Device", false, 0xFE, 1024);
        unknown.removal_policy = Some(2);
        assert!(drive_from_raw(&unknown, &BTreeSet::new())
            .unwrap()
            .removable());

        let mut sata = raw(3, "Samsung", "870 EVO", false, 0x0B, 1024);
        sata.removal_policy = Some(2);
        assert!(!drive_from_raw(&sata, &BTreeSet::new()).unwrap().removable());
    }

    #[test]
    fn a_drive_takes_the_physical_drive_path_that_windows_uses() {
        let d =
            drive_from_raw(&raw(2, "SanDisk", "Ultra", true, 7, 1024), &BTreeSet::new()).unwrap();
        assert_eq!(d.node, r"\\.\PhysicalDrive2");
        assert_eq!(d.id.as_str(), "2");
    }

    #[test]
    fn a_usb_solid_state_drive_reads_as_removable_although_windows_says_it_is_not() {
        // Windows reports RemovableMedia false, because no medium comes out.
        // The bus says USB, and a person unplugs it.
        let d = drive_from_raw(
            &raw(1, "Samsung", "T7", false, 0x07, 1024),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(!d.removable_media);
        assert!(d.removable());
    }

    #[test]
    fn an_internal_drive_is_not_removable() {
        let d = drive_from_raw(
            &raw(0, "Samsung", "980 PRO", false, 0x11, 1024),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(!d.removable());
        assert_eq!(d.bus, Bus::Nvme);
    }

    #[test]
    fn the_system_disk_is_marked_by_its_number() {
        let system: BTreeSet<u32> = [0].into_iter().collect();
        let d = drive_from_raw(&raw(0, "Samsung", "980 PRO", false, 0x11, 1024), &system).unwrap();
        assert!(d.system);
        let other = drive_from_raw(&raw(1, "SanDisk", "Ultra", true, 7, 1024), &system).unwrap();
        assert!(!other.system);
    }

    #[test]
    fn a_4096_byte_drive_takes_its_sector_size_from_the_alignment_answer() {
        let mut disk = raw(0, "Samsung", "PM9A3", false, 0x11, 4096);
        let mut alignment = vec![0u8; 24];
        alignment[16..20].copy_from_slice(&4096u32.to_le_bytes());
        alignment[20..24].copy_from_slice(&4096u32.to_le_bytes());
        disk.alignment = Some(alignment);
        let d = drive_from_raw(&disk, &BTreeSet::new()).unwrap();
        assert_eq!(d.logical_sector_size, 4096);
        assert_eq!(d.physical_sector_size, Some(4096));
    }

    #[test]
    fn a_driver_that_refuses_the_alignment_still_gives_a_drive() {
        let d = drive_from_raw(&raw(0, "A", "B", false, 0x0B, 1024), &BTreeSet::new()).unwrap();
        assert_eq!(d.logical_sector_size, 512);
        assert_eq!(d.physical_sector_size, None);
    }

    #[test]
    fn one_bad_disk_does_not_stop_the_list() {
        // A card reader with no card refuses the geometry call, and its
        // buffer comes back empty.
        let good = raw(0, "Samsung", "980 PRO", false, 0x11, 1024);
        let mut bad = raw(1, "Generic", "Reader", true, 0x0C, 0);
        bad.geometry = Vec::new();
        let drives = list_drives(&[good, bad], &BTreeSet::new());
        assert_eq!(drives.len(), 1);
        assert_eq!(drives[0].id.as_str(), "0");
    }

    #[test]
    fn a_disk_of_no_size_is_left_out() {
        let empty = raw(3, "Generic", "Reader", true, 0x0C, 0);
        assert!(list_drives(&[empty], &BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_disk_with_no_name_falls_back_to_its_number() {
        let d = drive_from_raw(&raw(4, "", "", false, 0, 1024), &BTreeSet::new()).unwrap();
        assert_eq!(d.name, "PhysicalDrive4");
    }

    #[test]
    fn at_512_bytes_the_table_is_34_sectors_at_the_start_and_33_at_the_end() {
        // The size of the stick that Windows refused to write.
        let length = 128_320_801_792;
        assert_eq!(
            table_areas(length, 512),
            vec![(0, 34 * 512), (length - 33 * 512, 33 * 512)]
        );
    }

    #[test]
    fn no_run_reaches_a_partition_of_a_valid_gpt() {
        let length = 1u64 << 30;
        for sector in [512u64, 4096] {
            // The usable space as UEFI defines it: after LBA 0, the header at
            // LBA 1 and the entries, and before the backup entries and the
            // backup header in the last LBA.
            let last_lba = length / sector - 1;
            let entry_sectors = GPT_ENTRY_ARRAY_BYTES / sector;
            let first_usable = 2 + entry_sectors;
            let last_usable = last_lba - 1 - entry_sectors;

            let runs = table_areas(length, sector as u32);
            assert_eq!(runs.len(), 2);
            assert_eq!(runs[0], (0, first_usable * sector));
            assert_eq!(runs[1].0, (last_usable + 1) * sector);
            assert_eq!(runs[1].0 + runs[1].1, length);
        }
    }

    #[test]
    fn a_drive_too_small_for_both_runs_is_one_run() {
        assert_eq!(table_areas(67 * 512, 512), vec![(0, 67 * 512)]);
        assert_eq!(table_areas(68 * 512, 512).len(), 2);
    }

    #[test]
    fn a_sector_of_zero_gives_no_run() {
        assert!(table_areas(1 << 30, 0).is_empty());
    }
}
