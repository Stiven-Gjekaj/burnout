//! The Windows device layer, which asks SetupAPI and the IO control codes.

#[cfg(windows)]
pub mod host;
pub mod parse;
pub mod raw;
