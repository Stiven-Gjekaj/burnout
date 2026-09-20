//! The part of Burnout that no operating system changes.
//!
//! Every type here works on any target that reads, writes and seeks. A test
//! gives it a file, and the command line gives it a drive. That is the only
//! reason the tests of this project touch no device.

#![forbid(unsafe_code)]
