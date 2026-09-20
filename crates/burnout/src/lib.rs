//! The command line of Burnout, and the three device layers under it.
//!
//! The binary holds almost nothing. The work lives here so that a test can
//! reach it, because a test cannot reach into a binary crate.

pub mod platform;
