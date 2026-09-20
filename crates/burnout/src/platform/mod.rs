//! The three device layers, and the choice between them.
//!
//! Each host is two files.
//!
//! `host.rs` makes system calls and decides nothing. It turns what the host
//! said into plain Rust data and hands it on. It is gated to its own host, so
//! no test reaches it.
//!
//! `parse.rs` decides everything and makes no system call. It is compiled on
//! all three hosts, so its tests run three times on every change. Everything
//! that can be wrong lives there.

pub mod linux;
pub mod macos;
