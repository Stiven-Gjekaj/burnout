//! What the Windows shim brings back, as plain data.
//!
//! Every answer from an IO control is a buffer of bytes. This module holds
//! those buffers, so that the code which reads them compiles and is tested on
//! all three hosts.

/// One disk, as the host answered about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawDisk {
    /// The path that names the device to the driver.
    pub interface_path: String,
    /// The number in `\\.\PhysicalDrive<N>`.
    pub device_number: u32,
    /// The answer to `IOCTL_STORAGE_QUERY_PROPERTY`.
    pub device_descriptor: Vec<u8>,
    /// The answer to `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`.
    pub geometry: Vec<u8>,
    /// The answer to the alignment query, which some drivers refuse.
    pub alignment: Option<Vec<u8>>,
    /// The name that the device manager shows.
    pub friendly_name: Option<String>,
    /// `SPDRP_REMOVAL_POLICY`.
    pub removal_policy: Option<u32>,
}

/// One volume, as the host answered about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawVolume {
    /// The name that `FindFirstVolumeW` gave, with its trailing backslash.
    pub guid_path: String,
    /// The answer to `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`.
    ///
    /// A card reader with no card gives none, and that is not a failure.
    pub extents: Option<Vec<u8>>,
}
