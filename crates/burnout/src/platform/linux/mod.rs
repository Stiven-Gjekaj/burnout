//! The Linux device layer, which reads `sysfs`.
//!
//! `sysfs` is a set of text files that the kernel writes. Reading them needs
//! the standard library and nothing else, and it leaves every decision in a
//! pure function that the tests run on all three hosts.

pub mod parse;
pub mod source;
