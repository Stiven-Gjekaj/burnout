//! Where the Linux code reads `sysfs` from.
//!
//! The real host reads `/sys`. A test holds the `sysfs` of a machine that it
//! describes itself, so the code that decides anything runs on every host.

use std::io;

/// A tree of files that answers the three questions `sysfs` needs.
///
/// An implementation of this trait makes no decision. It reads, and it
/// reports what it read.
pub trait SysfsSource {
    /// The contents of one attribute file.
    fn read_file(&self, path: &str) -> io::Result<String>;

    /// The names inside one directory, in any order.
    fn read_dir(&self, path: &str) -> io::Result<Vec<String>>;

    /// Where one symbolic link points, as the kernel wrote it.
    fn read_link(&self, path: &str) -> io::Result<String>;
}

/// Read one attribute and trim the newline that `sysfs` puts on the end.
///
/// A missing file is not an error. Many attributes are absent on a drive that
/// does not carry them, and an NVMe drive has no `device/vendor` at all.
pub fn attribute(fs: &dyn SysfsSource, path: &str) -> Option<String> {
    let text = fs.read_file(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::BTreeMap;

    /// The `sysfs` of a machine that a test describes.
    ///
    /// A test builds the state it needs inside itself. It never reads the
    /// `sysfs` of the machine that runs the test, because that machine
    /// changes and the test would then pass or fail for a reason that has
    /// nothing to do with the code.
    #[derive(Default, Debug)]
    pub(crate) struct MapSysfs {
        files: BTreeMap<String, String>,
        dirs: BTreeMap<String, Vec<String>>,
        links: BTreeMap<String, String>,
    }

    impl MapSysfs {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn file(mut self, path: &str, contents: &str) -> Self {
            self.files.insert(path.to_string(), contents.to_string());
            self
        }

        pub(crate) fn dir(mut self, path: &str, names: &[&str]) -> Self {
            self.dirs.insert(
                path.to_string(),
                names.iter().map(|n| n.to_string()).collect(),
            );
            self
        }

        pub(crate) fn link(mut self, path: &str, target: &str) -> Self {
            self.links.insert(path.to_string(), target.to_string());
            self
        }
    }

    impl SysfsSource for MapSysfs {
        fn read_file(&self, path: &str) -> io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))
        }

        fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
            self.dirs
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))
        }

        fn read_link(&self, path: &str) -> io::Result<String> {
            self.links
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::MapSysfs;
    use super::*;

    #[test]
    fn an_attribute_loses_the_newline_that_sysfs_adds() {
        let fs = MapSysfs::new().file("/sys/block/sda/size", "1953525168\n");
        assert_eq!(attribute(&fs, "/sys/block/sda/size").unwrap(), "1953525168");
    }

    #[test]
    fn a_missing_attribute_is_absent_and_not_an_error() {
        let fs = MapSysfs::new();
        assert!(attribute(&fs, "/sys/block/nvme0n1/device/vendor").is_none());
    }

    #[test]
    fn an_attribute_of_only_spaces_is_absent() {
        // A SCSI drive pads the vendor to eight characters. A drive that
        // reports nothing gives eight spaces, which is not a name.
        let fs = MapSysfs::new().file("/sys/block/sda/device/vendor", "        \n");
        assert!(attribute(&fs, "/sys/block/sda/device/vendor").is_none());
    }
}
