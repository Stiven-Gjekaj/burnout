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
mod listing;
mod memory_target;
mod progress;
mod sha256;
mod target;

pub use alignment::{
    check_aligned, check_sector_size, is_aligned, round_up, sector_count, SECTOR_SIZES,
};
pub use drive::{Bus, Connection, DriveId, DriveInfo};
pub use error::{Error, Result};
pub use file_target::FileTarget;
pub use listing::{by_index, in_list_order, natural_cmp, DriveList};
pub use memory_target::MemoryTarget;
pub use progress::{Progress, ProgressEvent, Silent, Stage};
pub use sha256::{sha256, Digest, Sha256};
pub use target::{BlockTarget, DriveAccess};
