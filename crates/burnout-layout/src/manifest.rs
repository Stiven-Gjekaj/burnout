//! What a copy put onto a volume, with the digest of each file.
//!
//! The digests are what a later check compares against. Raw mode checks one
//! digest of the whole drive. Windows mode writes files, so it checks one
//! digest for each file.

use burnout_core::Digest;

use crate::tree::TreePath;

/// A file that went onto a volume, and the digest of the bytes that went in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopiedFile {
    pub path: TreePath,
    pub bytes: u64,
    pub digest: Digest,
}

/// Everything that a copy put onto a volume, in the order it went on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub dirs: Vec<TreePath>,
    pub files: Vec<CopiedFile>,
}
