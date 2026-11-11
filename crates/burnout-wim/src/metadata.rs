//! The metadata resource of one edition: its security descriptors and its
//! tree of directory entries.
//!
//! The layout follows the WIM format of Microsoft. The security data comes
//! first, then the root entry on the next 8-byte boundary. Each entry with
//! children gives the place of its first child, and the children follow one
//! another, each on an 8-byte boundary, until an entry of length 0.

use crate::sha1::Digest;

/// The fixed part of a directory entry, before its names.
const ENTRY_FIXED: usize = 0x66;
/// The fixed part of an entry of an extra stream, before its name.
const STREAM_FIXED: usize = 0x26;
/// The deepest tree that this reader follows. Windows has paths of some
/// tens of levels, so a deeper tree is damaged.
const MAX_DEPTH: usize = 512;

pub const ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// A stream of a file beside its main one, such as an alternate data
/// stream. An extra stream with no name holds the data of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraStream {
    pub name: String,
    pub hash: Digest,
}

/// One file or directory of an edition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dentry {
    /// The path from the root, with `/` between the names. The root is "".
    pub path: String,
    pub short_name: String,
    pub attributes: u32,
    /// The index of the security descriptor, or `None`.
    pub security_id: Option<u32>,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    /// The SHA-1 of the main stream, or zeros for an empty file.
    pub hash: Digest,
    /// The reparse tag, for a reparse point.
    pub reparse_tag: Option<u32>,
    /// The files that share an ID that is not 0 are hard links of one file.
    pub hard_link_group: u64,
    pub streams: Vec<ExtraStream>,
}

impl Dentry {
    pub fn is_directory(&self) -> bool {
        self.attributes & ATTRIBUTE_DIRECTORY != 0
    }

    /// The SHA-1 of the data of the file: the main stream, or an extra
    /// stream with no name when the main one is empty.
    pub fn data_hash(&self) -> Option<&Digest> {
        if self.hash != [0; 20] {
            return Some(&self.hash);
        }
        self.streams
            .iter()
            .find(|s| s.name.is_empty() && s.hash != [0; 20])
            .map(|s| &s.hash)
    }
}

/// The metadata of one edition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub security: Vec<Vec<u8>>,
    /// The root first, and then each entry before the entries inside it.
    pub entries: Vec<Dentry>,
}

/// Read the metadata of one edition from its decompressed bytes.
pub fn parse_metadata(bytes: &[u8]) -> Result<Metadata, String> {
    let (security, after) = security_data(bytes)?;
    let mut entries = Vec::new();
    let root_at = align8(after);
    let (root, root_children) =
        read_entry(bytes, root_at, "")?.ok_or("the metadata holds no root entry")?;
    entries.push(root);
    // Each item is the offset of the first child and the path of the parent.
    let mut todo: Vec<(u64, String, usize)> = Vec::new();
    if root_children != 0 {
        todo.push((root_children, String::new(), 1));
    }
    while let Some((mut at, parent, depth)) = todo.pop() {
        if depth > MAX_DEPTH {
            return Err(format!("the tree is deeper than {MAX_DEPTH} levels"));
        }
        let first = entries.len();
        loop {
            let at_usize = usize::try_from(at).map_err(|_| format!("an entry at byte {at}"))?;
            let Some((entry, children)) = read_entry(bytes, at_usize, &parent)? else {
                break;
            };
            if entry.path.rsplit('/').next() == Some("") {
                return Err(format!("an entry in {parent:?} has no name"));
            }
            let next = at_usize + entry_total(bytes, at_usize)?;
            if children != 0 {
                todo.push((children, entry.path.clone(), depth + 1));
            }
            entries.push(entry);
            if entries.len() > bytes.len() / 8 {
                return Err("the tree holds more entries than its bytes can".to_string());
            }
            at = next as u64;
        }
        entries[first..].sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(Metadata { security, entries })
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The security descriptors, and the byte where the security data ends.
fn security_data(b: &[u8]) -> Result<(Vec<Vec<u8>>, usize), String> {
    if b.len() < 8 {
        return Err("the metadata is shorter than its security data".to_string());
    }
    let total = u32_at(b, 0) as usize;
    let count = u32_at(b, 4) as usize;
    if total < 8 || total > b.len() || count > (total - 8) / 8 {
        return Err(format!(
            "the security data gives {count} descriptors in {total} bytes"
        ));
    }
    let mut at = 8 + 8 * count;
    let mut descriptors = Vec::with_capacity(count);
    for i in 0..count {
        let size = u64_at(b, 8 + 8 * i) as usize;
        let end = at
            .checked_add(size)
            .filter(|e| *e <= total)
            .ok_or_else(|| format!("security descriptor {i} ends past the security data"))?;
        descriptors.push(b[at..end].to_vec());
        at = end;
    }
    Ok((descriptors, total))
}

/// The UTF-16LE name of `n` bytes at `at`.
fn name_at(b: &[u8], at: usize, n: usize) -> Result<String, String> {
    if n % 2 != 0 {
        return Err(format!("a name of {n} bytes at byte {at}"));
    }
    let units: Vec<u16> = b[at..at + n]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| format!("the name at byte {at} is not UTF-16"))
}

/// The entry at `at`, and the place of its first child, or `None` for the
/// entry of length 0 that ends a list.
fn read_entry(b: &[u8], at: usize, parent: &str) -> Result<Option<(Dentry, u64)>, String> {
    let fault = |what: &str| format!("the entry at byte {at} {what}");
    if at + 8 > b.len() {
        return Err(fault("starts past the end of the metadata"));
    }
    let length = u64_at(b, at) as usize;
    if length <= 8 {
        return Ok(None);
    }
    if length < ENTRY_FIXED || at.checked_add(length).map_or(true, |e| e > b.len()) {
        return Err(fault(&format!("has a length of {length}")));
    }
    let e = &b[at..at + length];
    let attributes = u32_at(e, 0x08);
    let security = u32_at(e, 0x0C) as i32;
    let children = u64_at(e, 0x10);
    let reparse = attributes & ATTRIBUTE_REPARSE_POINT != 0;
    let stream_count = u16_at(e, 0x60) as usize;
    let short_bytes = u16_at(e, 0x62) as usize;
    let name_bytes = u16_at(e, 0x64) as usize;
    let mut cursor = ENTRY_FIXED;
    let names_end = cursor
        + name_bytes
        + if name_bytes > 0 { 2 } else { 0 }
        + short_bytes
        + if short_bytes > 0 { 2 } else { 0 };
    if names_end > length {
        return Err(fault("has names longer than the entry"));
    }
    let name = name_at(e, cursor, name_bytes)?;
    if name.contains('/') || name.contains('\0') {
        return Err(fault(&format!("has the name {name:?}")));
    }
    cursor += name_bytes + if name_bytes > 0 { 2 } else { 0 };
    let short_name = name_at(e, cursor, short_bytes)?;

    let mut streams = Vec::with_capacity(stream_count);
    let mut s = align8(at + length);
    for _ in 0..stream_count {
        if s + STREAM_FIXED > b.len() {
            return Err(fault("has an extra stream past the end of the metadata"));
        }
        let slen = u64_at(b, s) as usize;
        let sname_bytes = u16_at(b, s + 0x24) as usize;
        if slen < STREAM_FIXED
            || s.checked_add(slen).map_or(true, |end| end > b.len())
            || STREAM_FIXED + sname_bytes > slen
        {
            return Err(fault("has an extra stream that its length cannot hold"));
        }
        streams.push(ExtraStream {
            name: name_at(b, s + STREAM_FIXED, sname_bytes)?,
            hash: b[s + 0x10..s + 0x24].try_into().unwrap(),
        });
        s = align8(s + slen);
    }

    let path = match (parent, name.is_empty()) {
        ("", _) => name,
        (p, _) => format!("{p}/{name}"),
    };
    let entry = Dentry {
        path,
        short_name,
        attributes,
        security_id: (security >= 0).then_some(security as u32),
        creation_time: u64_at(e, 0x28),
        last_access_time: u64_at(e, 0x30),
        last_write_time: u64_at(e, 0x38),
        hash: e[0x40..0x54].try_into().unwrap(),
        reparse_tag: reparse.then(|| u32_at(e, 0x58)),
        hard_link_group: if reparse { 0 } else { u64_at(e, 0x58) },
        streams,
    };
    Ok(Some((entry, children)))
}

/// The bytes of the entry at `at` with its extra streams, to the next one.
fn entry_total(b: &[u8], at: usize) -> Result<usize, String> {
    let length = u64_at(b, at) as usize;
    let count = u16_at(b, at + 0x60) as usize;
    let mut end = align8(at + length);
    for _ in 0..count {
        end = align8(end + u64_at(b, end) as usize);
    }
    Ok(end - at)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An entry with this name, these attributes, this main hash, these
    /// extra streams and the place of its first child.
    pub(crate) fn entry(
        name: &str,
        attributes: u32,
        hash: Digest,
        streams: &[(&str, Digest)],
        children: u64,
    ) -> Vec<u8> {
        let name16: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut e = vec![0u8; ENTRY_FIXED];
        e[0x08..0x0C].copy_from_slice(&attributes.to_le_bytes());
        e[0x0C..0x10].copy_from_slice(&(-1i32).to_le_bytes());
        e[0x10..0x18].copy_from_slice(&children.to_le_bytes());
        e[0x38..0x40].copy_from_slice(&7u64.to_le_bytes());
        e[0x40..0x54].copy_from_slice(&hash);
        e[0x58..0x60].copy_from_slice(&5u64.to_le_bytes());
        e[0x60..0x62].copy_from_slice(&(streams.len() as u16).to_le_bytes());
        e[0x64..0x66].copy_from_slice(&(name16.len() as u16).to_le_bytes());
        e.extend_from_slice(&name16);
        if !name16.is_empty() {
            e.extend_from_slice(&[0, 0]);
        }
        let length = e.len() as u64;
        e[..8].copy_from_slice(&length.to_le_bytes());
        e.resize(align8(e.len()), 0);
        for (sname, shash) in streams {
            let n16: Vec<u8> = sname.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let mut s = vec![0u8; STREAM_FIXED];
            s[0x10..0x24].copy_from_slice(shash);
            s[0x24..0x26].copy_from_slice(&(n16.len() as u16).to_le_bytes());
            s.extend_from_slice(&n16);
            if !n16.is_empty() {
                s.extend_from_slice(&[0, 0]);
            }
            let slen = s.len() as u64;
            s[..8].copy_from_slice(&slen.to_le_bytes());
            s.resize(align8(s.len()), 0);
            e.extend_from_slice(&s);
        }
        e
    }

    /// A metadata resource: one descriptor of 4 bytes, then the root with
    /// the directory "dir", which holds "b.txt" and "a.txt", and the file
    /// "top.txt" beside it.
    fn sample() -> Vec<u8> {
        let mut m = Vec::new();
        m.extend_from_slice(&24u32.to_le_bytes());
        m.extend_from_slice(&1u32.to_le_bytes());
        m.extend_from_slice(&4u64.to_le_bytes());
        m.extend_from_slice(&[1, 2, 3, 4]);
        m.resize(align8(m.len()), 0);
        let root_at = m.len();
        // The root, its list of children, and the list of "dir".
        let root = entry("", ATTRIBUTE_DIRECTORY, [0; 20], &[], 0);
        let top = entry("top.txt", 0x20, [3; 20], &[("ads", [4; 20])], 0);
        let dir_len = entry("dir", ATTRIBUTE_DIRECTORY, [0; 20], &[], 0).len();
        let children = root_at + root.len();
        let dir_children = children + dir_len + top.len() + 8;
        let mut root = root;
        root[0x10..0x18].copy_from_slice(&(children as u64).to_le_bytes());
        m.extend_from_slice(&root);
        m.extend_from_slice(&entry(
            "dir",
            ATTRIBUTE_DIRECTORY,
            [0; 20],
            &[],
            dir_children as u64,
        ));
        m.extend_from_slice(&top);
        m.extend_from_slice(&[0; 8]);
        m.extend_from_slice(&entry("b.txt", 0x20, [2; 20], &[], 0));
        m.extend_from_slice(&entry("a.txt", 0x20, [0; 20], &[("", [1; 20])], 0));
        m.extend_from_slice(&[0; 8]);
        m
    }

    #[test]
    fn the_tree_comes_out_with_its_paths_streams_and_security() {
        let m = parse_metadata(&sample()).unwrap();
        assert_eq!(m.security, vec![vec![1, 2, 3, 4]]);
        let paths: Vec<&str> = m.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["", "dir", "top.txt", "dir/a.txt", "dir/b.txt"]);
        let top = &m.entries[2];
        assert!(!top.is_directory());
        assert_eq!(top.data_hash(), Some(&[3; 20]));
        assert_eq!(
            top.streams,
            [ExtraStream {
                name: "ads".to_string(),
                hash: [4; 20]
            }]
        );
        assert_eq!(top.security_id, None);
        assert_eq!(top.last_write_time, 7);
        assert_eq!(top.hard_link_group, 5);
        assert_eq!(top.reparse_tag, None);
        // The data of a.txt is in an extra stream with no name.
        assert_eq!(m.entries[3].data_hash(), Some(&[1; 20]));
        assert!(m.entries[1].is_directory());
        assert_eq!(m.entries[1].data_hash(), None);
    }

    #[test]
    fn a_reparse_point_gives_its_tag_and_no_link_group() {
        let mut m = sample();
        // top.txt becomes a reparse point.
        let root_at = 24;
        let children = u64_at(&m, root_at + 0x10) as usize;
        let top = children + entry_total(&m, children).unwrap();
        m[top + 0x08..top + 0x0C].copy_from_slice(&(0x20 | ATTRIBUTE_REPARSE_POINT).to_le_bytes());
        let parsed = parse_metadata(&m).unwrap();
        let top = parsed.entries.iter().find(|e| e.path == "top.txt").unwrap();
        assert_eq!(top.reparse_tag, Some(5));
        assert_eq!(top.hard_link_group, 0);
    }

    #[test]
    fn damaged_metadata_says_what_is_wrong() {
        let good = sample();
        let mut cases: Vec<(Vec<u8>, &str)> = vec![
            (vec![0; 4], "shorter than its security data"),
            (
                {
                    let mut m = good.clone();
                    m[0] = 4;
                    m
                },
                "descriptors in 4 bytes",
            ),
            (
                {
                    let mut m = good.clone();
                    m[8] = 200;
                    m
                },
                "ends past the security data",
            ),
        ];
        let mut long = good.clone();
        long[24..32].copy_from_slice(&100_000u64.to_le_bytes());
        cases.push((long, "has a length of 100000"));
        let mut loop_ = good.clone();
        // The root lists itself as its child, so the tree has no end.
        loop_[24 + 0x10..24 + 0x18].copy_from_slice(&24u64.to_le_bytes());
        cases.push((loop_, "has no name"));
        for (bytes, says) in cases {
            let e = parse_metadata(&bytes).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
    }
}
