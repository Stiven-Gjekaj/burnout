//! Read the image that a person gives Burnout.
//!
//! A Linux ISO keeps its tree in ISO 9660, with Rock Ridge and Joliet on top.
//! A Windows ISO keeps its tree in UDF, because ISO 9660 cannot hold a file of
//! 4 GiB or more. Burnout reads both itself, for the same reason it writes the
//! file systems of a drive itself: a mount through the host behaves in a
//! different way on each of three hosts. [The roadmap] records what the reader
//! covers, and the reason for each decision.
//!
//! [The roadmap]: https://github.com/Stiven-Gjekaj/burnout/blob/main/docs/roadmap.md

#![forbid(unsafe_code)]

mod image;
mod iso9660;
mod node;
mod source;
#[cfg(test)]
mod testing;
mod udf;

pub use source::{FileSystem, IsoSource, Link};
