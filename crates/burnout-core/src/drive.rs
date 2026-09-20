//! What Burnout knows about one drive.
//!
//! Three operating systems report a drive in three shapes. This module holds
//! the one shape that the rest of Burnout reads, so that the code above the
//! device layer never asks which host it runs on.

use std::fmt;

/// The name that the host gives one whole drive.
///
/// This is not the target that a person types. A person types a number, and
/// [`crate::listing`] turns the number into one of these.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DriveId(String);

impl DriveId {
    /// Take the name that the host uses, such as `sda`, `disk4` or `2`.
    pub fn new(name: impl Into<String>) -> Self {
        DriveId(name.into())
    }

    /// Give the name back.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The kind of connection that carries the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Bus {
    Usb,
    Sata,
    Ata,
    Sas,
    Scsi,
    Nvme,
    PciExpress,
    Sd,
    Mmc,
    FireWire,
    Thunderbolt,
    AppleFabric,
    Raid,
    /// A disk image, or a disk that a virtual machine makes.
    Virtual,
    /// The host gave a value that this code does not know.
    Unknown,
}

impl Bus {
    /// The word that the `BUS` column prints.
    pub fn label(self) -> &'static str {
        match self {
            Bus::Usb => "USB",
            Bus::Sata => "SATA",
            Bus::Ata => "ATA",
            Bus::Sas => "SAS",
            Bus::Scsi => "SCSI",
            Bus::Nvme => "NVMe",
            Bus::PciExpress => "PCIe",
            Bus::Sd => "SD",
            Bus::Mmc => "MMC",
            Bus::FireWire => "FireWire",
            Bus::Thunderbolt => "TB",
            Bus::AppleFabric => "Fabric",
            Bus::Raid => "RAID",
            Bus::Virtual => "Virtual",
            Bus::Unknown => "?",
        }
    }
}

impl fmt::Display for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Where the drive sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Connection {
    /// Inside the computer.
    Internal,
    /// On a cable or a port that a person reaches.
    External,
    /// The host did not say.
    Unknown,
}

/// One whole drive, as the rest of Burnout reads it.
///
/// A partition is not a drive. This describes the whole device only.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DriveInfo {
    /// The name that the host uses for the whole drive.
    pub id: DriveId,
    /// The path that opens the drive on this host.
    ///
    /// On macOS this is the raw node, `/dev/rdiskN`, and never `/dev/diskN`.
    /// The raw node skips the buffer cache.
    pub node: String,
    /// The name that the `DRIVE` column prints.
    pub name: String,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// The size of the drive in bytes, and not in sectors.
    pub size_bytes: u64,
    /// The smallest write that the drive accepts, in bytes.
    pub logical_sector_size: u32,
    /// The size of one sector on the medium, when the host reports it.
    pub physical_sector_size: Option<u32>,
    pub bus: Bus,
    pub connection: Connection,
    /// The host says that the medium comes out of the drive.
    ///
    /// Read [`DriveInfo::removable`] instead of this field. The three hosts
    /// do not agree about the word.
    pub removable_media: bool,
    /// The running system starts from this drive.
    pub system: bool,
}

impl DriveInfo {
    /// Make a drive with the fields that every host reports, and the
    /// defaults for the rest.
    pub fn new(id: DriveId, node: impl Into<String>, name: impl Into<String>) -> Self {
        DriveInfo {
            id,
            node: node.into(),
            name: name.into(),
            vendor: None,
            model: None,
            serial: None,
            size_bytes: 0,
            logical_sector_size: 512,
            physical_sector_size: None,
            bus: Bus::Unknown,
            connection: Connection::Unknown,
            removable_media: false,
            system: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_id_gives_its_name_back() {
        let id = DriveId::new("disk4");
        assert_eq!(id.as_str(), "disk4");
        assert_eq!(id.to_string(), "disk4");
    }

    #[test]
    fn every_bus_prints_a_short_label() {
        for bus in [Bus::Usb, Bus::Nvme, Bus::Sata, Bus::Virtual, Bus::Unknown] {
            assert!(!bus.label().is_empty());
            assert!(bus.label().len() <= 8, "{bus} is too wide for the column");
        }
    }

    #[test]
    fn a_new_drive_takes_the_smaller_sector_size() {
        let d = DriveInfo::new(DriveId::new("sda"), "/dev/sda", "Samsung SSD");
        assert_eq!(d.logical_sector_size, 512);
        assert_eq!(d.size_bytes, 0);
        assert!(!d.system);
    }
}
