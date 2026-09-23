//! ISO 9660, the file system of a disc, with Joliet and Rock Ridge.
//!
//! A Linux ISO keeps its tree here. Plain ISO 9660 keeps short names in upper
//! case. Joliet adds a second tree with longer names in UTF-16, and Rock Ridge
//! adds the names, modes and links of a POSIX system to the first tree.

mod descriptors;

pub(crate) use descriptors::{read_descriptors, Descriptors, Root, Tree};
