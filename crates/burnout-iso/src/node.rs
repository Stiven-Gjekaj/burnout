//! A directory, a file or a link of an image, and where its bytes are.
//!
//! Each file system of an image gives its tree in this one shape, so the tree
//! that Burnout copies does not know which file system it came from.

use burnout_core::TreePath;

/// A run of bytes of the image that holds a part of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    /// The first byte, counted from the start of the image.
    pub at: u64,
    pub length: u64,
    /// A run that the file system records as never written. It reads as
    /// zero, and the image holds no byte of it.
    pub zero: bool,
}

/// What one entry of a tree is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
    /// A link, and the path that it names.
    Link(String),
}

/// One entry of the tree of an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub path: TreePath,
    pub kind: Kind,
    /// The bytes of a file. A directory and a link count zero.
    pub size: u64,
    /// Where the bytes of a file are, in order.
    pub extents: Vec<Extent>,
}
