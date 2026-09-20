//! The macOS device layer, which reads the IOKit registry.

#[cfg(target_os = "macos")]
pub mod host;
pub mod model;
pub mod parse;
