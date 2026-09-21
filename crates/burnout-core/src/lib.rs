//! The part of Burnout that no operating system changes.
//!
//! Every type here works on any target that reads, writes and seeks. A test
//! gives it a file, and the command line gives it a drive. That is the only
//! reason the tests of this project touch no device.

#![forbid(unsafe_code)]

mod alignment;
mod drive;
mod error;
mod file_target;
mod image;
mod listing;
mod memory_target;
mod progress;
mod safety;
mod sha256;
mod strict_target;
mod target;
mod write;

pub use alignment::{
    check_aligned, check_sector_size, is_aligned, round_up, sector_count, SECTOR_SIZES,
};
pub use drive::{Bus, Connection, DriveId, DriveInfo};
pub use error::{Error, Result};
pub use file_target::FileTarget;
pub use image::{has_boot_table, BOOT_SECTOR_BYTES};
pub use listing::{by_index, in_list_order, natural_cmp, DriveList};
pub use memory_target::MemoryTarget;
pub use progress::{Progress, ProgressEvent, Silent, Stage};
pub use safety::{
    check_fits, check_same_drive, check_target, describe, force_phrase, phrase_matches, Force,
};
pub use sha256::{sha256, Digest, Sha256};
pub use strict_target::StrictTarget;
pub use target::{BlockTarget, DriveAccess};
pub use write::{verify_image, write_image, Verification, WriteReport};
