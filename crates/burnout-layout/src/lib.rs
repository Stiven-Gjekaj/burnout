//! The layout of a Windows installer drive.
//!
//! One MBR partition table and two partitions: FAT32 for every boot file,
//! and exFAT for the install image, whole. [The milestones] give the reason
//! for each part of that.
//!
//! Everything here works on a [`burnout_core::BlockTarget`], so a test gives
//! it a file or memory and the product gives it a drive. The code is the same
//! in both, which is the only reason a test of it means anything.
//!
//! This is the one crate of the project that depends on `fatfs`, so
//! `burnout-core` keeps no dependency at all.
//!
//! [The milestones]: https://github.com/Stiven-Gjekaj/burnout/blob/main/docs/milestones.md

mod copy;
mod dot_entries;
mod exfat;
mod fat32;
mod manifest;
mod mbr;
mod plan;
mod stream;
#[cfg(test)]
mod testing;
mod unattend;
mod verify;

pub use burnout_core::{DirSource, Entry, FileSource, MemorySource, TreePath};
pub use copy::{copy_to_fat32, copy_to_fat32_with, MAX_FAT32_FILE_BYTES};
pub use exfat::{verify_exfat, verify_exfat_with, write_exfat, write_exfat_with, ExfatOptions};
pub use fat32::{format_fat32, Fat32Options};
pub use manifest::{CopiedFile, Manifest};
pub use mbr::{mbr_bytes, write_table, TYPE_EXFAT, TYPE_FAT32};
pub use plan::{plan, Extent, Layout, ALIGN, MBR_SECTORS};
pub use unattend::{unattend_xml, Architecture, InstallImage, Unattend};
pub use verify::{verify_fat32, verify_fat32_with};
