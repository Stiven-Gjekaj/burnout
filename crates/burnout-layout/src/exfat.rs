//! The exFAT volume on partition 2, which holds the install image whole.
//!
//! No crate writes exFAT, so this does, from the specification that Microsoft
//! published in 2019. [The roadmap] records how the writer works, and the
//! reason for each decision.
//!
//! [The roadmap]: https://github.com/Stiven-Gjekaj/burnout/blob/main/docs/roadmap.md

mod boot;
mod entries;
mod geometry;
mod place;
mod read;
mod sums;
mod upcase;
mod verify;
mod write;

pub use verify::verify_exfat;
pub use write::{write_exfat, ExfatOptions};
