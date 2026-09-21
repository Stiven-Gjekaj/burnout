//! A tree of files, which Windows mode copies onto a volume.
//!
//! Raw mode copies one image, byte for byte. Windows mode copies files. In
//! the product they come from the ISO, and in a test from a directory or from
//! memory. Each source has this one shape, so the copy does not know which
//! one it reads.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use burnout_core::{Error, Result};

/// A path inside a tree, from its root, with `/` between the names.
///
/// No name is empty, `.` or `..`, so a path cannot point outside the tree.
///
/// Paths sort one name at a time. A directory then comes before everything
/// in it, and everything in it comes before the next name, which is the
/// order in which a walk of the tree finds them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TreePath(String);

impl TreePath {
    pub fn new(path: &str) -> Result<Self> {
        let good = |name: &str| !name.is_empty() && name != "." && name != "..";
        if path.split('/').all(good) {
            Ok(TreePath(path.to_string()))
        } else {
            Err(Error::Source {
                path: path.to_string(),
                detail:
                    "a path of a tree has a name between each two `/`, and no name is `.` or `..`"
                        .to_string(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The names from the top of the tree down.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The directory that holds this, or `None` at the top of the tree.
    pub fn parent(&self) -> Option<TreePath> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| TreePath(parent.to_string()))
    }

    /// The path of `name` inside this directory.
    pub fn join(&self, name: &str) -> Result<TreePath> {
        TreePath::new(&format!("{}/{name}", self.0))
    }
}

impl Ord for TreePath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.names().cmp(other.names())
    }
}

impl PartialOrd for TreePath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for TreePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One thing in a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A directory.
    Dir(TreePath),
    /// A file, and how many bytes it holds.
    File(TreePath, u64),
}

impl Entry {
    pub fn path(&self) -> &TreePath {
        match self {
            Entry::Dir(path) | Entry::File(path, _) => path,
        }
    }
}

/// A tree of files to copy onto a volume.
pub trait FileSource {
    /// Every directory and every file of the tree, in the order of
    /// [`TreePath`], so a directory comes before what it holds.
    ///
    /// The top of the tree is not an entry. The order is the same on each
    /// call and on each host, so the same tree makes the same volume.
    fn entries(&self) -> Result<Vec<Entry>>;

    /// Open a file of the tree, to read it from its first byte.
    fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>>;
}

/// A tree of files held in memory.
#[derive(Clone, Debug, Default)]
pub struct MemorySource {
    dirs: BTreeSet<TreePath>,
    files: BTreeMap<TreePath, Vec<u8>>,
}

impl MemorySource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file, and each directory above it that is not there yet.
    pub fn add_file(&mut self, path: &str, bytes: impl Into<Vec<u8>>) -> Result<&mut Self> {
        let path = TreePath::new(path)?;
        self.add_parents(&path)?;
        if self.dirs.contains(&path) {
            return Err(taken(&path, "a directory"));
        }
        self.files.insert(path, bytes.into());
        Ok(self)
    }

    /// Add a directory, and each directory above it that is not there yet.
    pub fn add_dir(&mut self, path: &str) -> Result<&mut Self> {
        let path = TreePath::new(path)?;
        self.add_parents(&path)?;
        if self.files.contains_key(&path) {
            return Err(taken(&path, "a file"));
        }
        self.dirs.insert(path);
        Ok(self)
    }

    fn add_parents(&mut self, path: &TreePath) -> Result<()> {
        let mut parent = path.parent();
        while let Some(dir) = parent {
            if self.files.contains_key(&dir) {
                return Err(taken(&dir, "a file"));
            }
            parent = dir.parent();
            self.dirs.insert(dir);
        }
        Ok(())
    }
}

/// The error for a name that the tree already gives to something else.
fn taken(path: &TreePath, what: &str) -> Error {
    Error::Source {
        path: path.to_string(),
        detail: format!("the tree already holds {what} with this name"),
    }
}

impl FileSource for MemorySource {
    fn entries(&self) -> Result<Vec<Entry>> {
        let dirs = self.dirs.iter().map(|dir| Entry::Dir(dir.clone()));
        let files = self
            .files
            .iter()
            .map(|(path, bytes)| Entry::File(path.clone(), bytes.len() as u64));
        let mut entries: Vec<Entry> = dirs.chain(files).collect();
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(entries)
    }

    fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>> {
        match self.files.get(path) {
            Some(bytes) => Ok(Box::new(bytes.as_slice())),
            None => Err(Error::Source {
                path: path.to_string(),
                detail: "the tree holds no file with this name".to_string(),
            }),
        }
    }
}

/// A directory of the host, as a tree.
///
/// A link is refused, and not followed. A link can point outside the
/// directory, or at a directory above itself, and then a walk never ends.
#[derive(Clone, Debug)]
pub struct DirSource {
    root: PathBuf,
}

impl DirSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DirSource { root: root.into() }
    }

    fn host_path(&self, path: &TreePath) -> PathBuf {
        let mut host = self.root.clone();
        host.extend(path.names());
        host
    }

    fn walk(&self, dir: &Path, above: Option<&TreePath>, out: &mut Vec<Entry>) -> Result<()> {
        for item in fs::read_dir(dir).map_err(|e| unreadable(dir, e))? {
            let item = item.map_err(|e| unreadable(dir, e))?;
            let host = item.path();
            let name = item.file_name().into_string().map_err(|_| Error::Source {
                path: host.display().to_string(),
                detail: "the name is not valid Unicode".to_string(),
            })?;
            let path = match above {
                Some(above) => above.join(&name)?,
                None => TreePath::new(&name)?,
            };
            // The type of the entry itself. A link is a link here, whatever
            // it points at.
            let kind = item.file_type().map_err(|e| unreadable(&host, e))?;
            if kind.is_dir() {
                out.push(Entry::Dir(path.clone()));
                self.walk(&host, Some(&path), out)?;
            } else if kind.is_file() {
                let bytes = item.metadata().map_err(|e| unreadable(&host, e))?.len();
                out.push(Entry::File(path, bytes));
            } else {
                return Err(Error::Source {
                    path: host.display().to_string(),
                    detail: "it is not a file and not a directory".to_string(),
                });
            }
        }
        Ok(())
    }
}

fn unreadable(host: &Path, e: io::Error) -> Error {
    Error::Source {
        path: host.display().to_string(),
        detail: e.to_string(),
    }
}

impl FileSource for DirSource {
    fn entries(&self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        self.walk(&self.root, None, &mut entries)?;
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(entries)
    }

    fn open(&self, path: &TreePath) -> Result<Box<dyn Read + '_>> {
        let host = self.host_path(path);
        let file = fs::File::open(&host).map_err(|e| unreadable(&host, e))?;
        Ok(Box::new(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory in the temporary directory that goes away with the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNT: AtomicU32 = AtomicU32::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            let name = format!("burnout-{}-{}-{}", std::process::id(), tag, n);
            let dir = std::env::temp_dir().join(name);
            fs::create_dir(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self, path: &str, bytes: &[u8]) {
            let host = self.0.join(path);
            fs::create_dir_all(host.parent().unwrap()).unwrap();
            fs::write(host, bytes).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn read_all(source: &dyn FileSource, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        let path = TreePath::new(path).unwrap();
        source.open(&path).unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn dir(path: &str) -> Entry {
        Entry::Dir(TreePath::new(path).unwrap())
    }

    fn file(path: &str, bytes: u64) -> Entry {
        Entry::File(TreePath::new(path).unwrap(), bytes)
    }

    #[test]
    fn a_path_has_a_name_between_each_two_slashes() {
        for good in ["a", "efi/boot/bootx64.efi", "a b/c.d"] {
            assert!(TreePath::new(good).is_ok(), "{good}");
        }
        for bad in ["", "/a", "a/", "a//b", ".", "..", "a/./b", "a/../b"] {
            assert!(TreePath::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_directory_comes_before_what_it_holds_and_that_before_the_next_name() {
        // `-` sorts before `/` as a byte, so a plain sort of the strings
        // puts `a-x` between `a` and `a/b`. A walk never does.
        let mut paths: Vec<TreePath> = ["b", "a-x", "a/b/c", "a", "a/b"]
            .into_iter()
            .map(|p| TreePath::new(p).unwrap())
            .collect();
        paths.sort();
        let names: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
        assert_eq!(names, ["a", "a/b", "a/b/c", "a-x", "b"]);
    }

    #[test]
    fn a_file_brings_the_directories_above_it() {
        let mut tree = MemorySource::new();
        tree.add_file("efi/boot/bootx64.efi", *b"MZ").unwrap();
        assert_eq!(
            tree.entries().unwrap(),
            [dir("efi"), dir("efi/boot"), file("efi/boot/bootx64.efi", 2)]
        );
    }

    #[test]
    fn a_file_and_a_directory_cannot_share_a_name() {
        let mut tree = MemorySource::new();
        tree.add_file("a", *b"x").unwrap();
        assert!(tree.add_dir("a").is_err());
        assert!(tree.add_file("a/b", *b"x").is_err());

        let mut tree = MemorySource::new();
        tree.add_dir("a").unwrap();
        assert!(tree.add_file("a", *b"x").is_err());
    }

    #[test]
    fn a_file_in_memory_reads_back_the_bytes_it_holds() {
        let mut tree = MemorySource::new();
        tree.add_file("sources/boot.wim", vec![7u8; 5000]).unwrap();
        assert_eq!(read_all(&tree, "sources/boot.wim"), vec![7u8; 5000]);
        assert!(tree.open(&TreePath::new("sources").unwrap()).is_err());
    }

    #[test]
    fn a_directory_of_the_host_is_the_same_tree_as_the_one_in_memory() {
        let s = Scratch::new("tree");
        s.file("setup.exe", b"setup");
        s.file("efi/boot/bootx64.efi", b"MZ");
        s.file("a-x", b"");
        fs::create_dir(s.0.join("empty")).unwrap();

        let mut memory = MemorySource::new();
        memory.add_file("setup.exe", *b"setup").unwrap();
        memory.add_file("efi/boot/bootx64.efi", *b"MZ").unwrap();
        memory.add_file("a-x", *b"").unwrap();
        memory.add_dir("empty").unwrap();

        let host = DirSource::new(&s.0);
        assert_eq!(host.entries().unwrap(), memory.entries().unwrap());
        assert_eq!(read_all(&host, "efi/boot/bootx64.efi"), b"MZ");
        assert_eq!(read_all(&host, "setup.exe"), b"setup");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_a_directory_of_the_host_is_refused() {
        let s = Scratch::new("link");
        s.file("real", b"x");
        std::os::unix::fs::symlink(s.0.join("real"), s.0.join("link")).unwrap();
        let e = DirSource::new(&s.0).entries().unwrap_err();
        assert!(
            e.to_string().contains("not a file and not a directory"),
            "{e}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_unicode_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let s = Scratch::new("name");
        let name = std::ffi::OsStr::from_bytes(b"caf\xe9");
        fs::write(s.0.join(name), b"x").unwrap();
        let e = DirSource::new(&s.0).entries().unwrap_err();
        assert!(e.to_string().contains("Unicode"), "{e}");
    }
}
