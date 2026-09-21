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
