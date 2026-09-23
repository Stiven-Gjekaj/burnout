//! UDF, the file system that holds the files of a Windows ISO.
//!
//! The ISO 9660 side of a Windows ISO holds only a note that tells a person
//! to use a reader of UDF. UDF finds its volume from an anchor at block 256,
//! then a sequence of volume descriptors, a partition and a file set. The
//! root of the file set is the tree.

mod tag;

pub(crate) use tag::{crc_itu_t, read_tag, Tag};
