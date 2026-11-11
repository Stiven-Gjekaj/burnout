//! The index of a directory, and the other indexes of NTFS: a B-tree whose
//! root is the attribute $INDEX_ROOT in the record, and whose other nodes
//! are blocks of $INDEX_ALLOCATION, with $BITMAP to say which blocks are in
//! use.
//!
//! The tree is built from the bottom: the entries, in order, fill blocks
//! from the left, and the entry that does not fit goes one level up, with
//! the full block as its child. When the entries of a level fit in the
//! root, the root holds them. The layout of a block is the one that format
//! of Windows 11 25H2 writes, measured on 2026-11-11.

use crate::ntfs::record::apply_fixups;

/// The flags of an entry.
const ENTRY_HAS_CHILD: u32 = 0x01;
const ENTRY_LAST: u32 = 0x02;
/// The flag of a node that has children.
const NODE_HAS_CHILDREN: u32 = 0x01;
/// The node of a block starts here, after the block header.
const NODE_AT: usize = 0x18;
/// The first entry of a block, from the start of the node, as Windows puts
/// it.
const BLOCK_ENTRIES: usize = 0x40;
const BLOCK_USA_AT: usize = 0x28;
/// The first entry of the root, from the start of the node.
const ROOT_ENTRIES: usize = 0x10;

/// One entry of an index: the file that it names, or 0, and its key. For a
/// directory the key is the $FILE_NAME of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub file: u64,
    pub key: Vec<u8>,
    /// The data after the key, which a view index such as $SDH holds.
    pub data: Vec<u8>,
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

impl Entry {
    /// The bytes of the entry, with the VCN of its child at the end.
    fn bytes(&self, child: Option<u64>, last: bool) -> Vec<u8> {
        let body = align8(16 + self.key.len() + self.data.len());
        let len = body + if child.is_some() { 8 } else { 0 };
        let mut b = vec![0u8; len];
        b[0..8].copy_from_slice(&self.file.to_le_bytes());
        if !self.data.is_empty() {
            // A view index puts the place and the size of the data where
            // a directory puts the file reference.
            let data_at = 16 + self.key.len();
            b[0..2].copy_from_slice(&(data_at as u16).to_le_bytes());
            b[2..4].copy_from_slice(&(self.data.len() as u16).to_le_bytes());
            b[4..8].fill(0);
            b[data_at..data_at + self.data.len()].copy_from_slice(&self.data);
        }
        b[8..10].copy_from_slice(&(len as u16).to_le_bytes());
        b[10..12].copy_from_slice(&(self.key.len() as u16).to_le_bytes());
        let mut flags = 0;
        if child.is_some() {
            flags |= ENTRY_HAS_CHILD;
        }
        if last {
            flags |= ENTRY_LAST;
        }
        b[12..16].copy_from_slice(&flags.to_le_bytes());
        b[16..16 + self.key.len()].copy_from_slice(&self.key);
        if let Some(vcn) = child {
            b[len - 8..].copy_from_slice(&vcn.to_le_bytes());
        }
        b
    }

    fn size(&self, with_child: bool) -> usize {
        align8(16 + self.key.len() + self.data.len()) + if with_child { 8 } else { 0 }
    }
}

/// The bytes of the end entry, which every node has.
fn end_entry(child: Option<u64>) -> Vec<u8> {
    Entry {
        file: 0,
        key: Vec::new(),
        data: Vec::new(),
    }
    .bytes(child, true)
}

/// A node: its entries, each with its child to the left, and the child of
/// its end entry. A child is the number of a block.
#[derive(Clone, Debug)]
struct Node {
    entries: Vec<(Entry, Option<usize>)>,
    end_child: Option<usize>,
}

impl Node {
    fn has_children(&self) -> bool {
        self.end_child.is_some()
    }

    /// The bytes of the entries and the end entry, with each child as a VCN.
    fn entry_bytes(&self, vcn: &dyn Fn(usize) -> u64) -> Vec<u8> {
        let mut b = Vec::new();
        for (e, child) in &self.entries {
            b.extend_from_slice(&e.bytes(child.map(vcn), false));
        }
        b.extend_from_slice(&end_entry(self.end_child.map(vcn)));
        b
    }

    fn size(&self) -> usize {
        node_size(self.entries.iter().map(|(e, _)| e), self.has_children())
    }
}

/// What the attributes of an index hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Index {
    /// The value of $INDEX_ROOT.
    pub root: Vec<u8>,
    /// The blocks of $INDEX_ALLOCATION, each of `block_bytes`, with fixups.
    /// None when the root holds each entry.
    pub blocks: Vec<Vec<u8>>,
    /// The value of $BITMAP: one bit for each block.
    pub bitmap: Vec<u8>,
}

/// The shape of an index.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shape {
    /// The type of attribute that the index keys on: 0x30 for file names,
    /// or 0 for a view index.
    pub key_kind: u32,
    /// The collation rule: 1 for file names, 0x10 for unsigned numbers,
    /// 0x13 for several unsigned numbers.
    pub collation: u32,
    pub block_bytes: usize,
    pub cluster_bytes: usize,
    /// The bytes that the root may take in its record.
    pub root_space: usize,
}

impl Shape {
    /// The VCN of block `n`.
    fn vcn(&self, n: usize) -> u64 {
        if self.block_bytes >= self.cluster_bytes {
            (n * self.block_bytes / self.cluster_bytes) as u64
        } else {
            (n * self.block_bytes / 512) as u64
        }
    }

    /// The size field of the root: clusters, or sectors when a block is
    /// smaller than a cluster.
    fn block_field(&self) -> u32 {
        if self.block_bytes >= self.cluster_bytes {
            (self.block_bytes / self.cluster_bytes) as u32
        } else {
            (self.block_bytes / 512) as u32
        }
    }

    /// The bytes that the entries of a block may take.
    fn block_space(&self) -> usize {
        self.block_bytes - NODE_AT - BLOCK_ENTRIES
    }
}

/// The bytes of a node that holds `entries`, with its end entry.
fn node_size<'a>(entries: impl Iterator<Item = &'a Entry>, children: bool) -> usize {
    let child = if children { 8 } else { 0 };
    entries.map(|e| e.size(children)).sum::<usize>() + 16 + child
}

/// Split one level of entries into nodes that each fit `space`. Give the
/// nodes and the entries that go up a level, each with the node to its left.
/// `children` says whether the entries of this level have children.
fn split(
    level: Vec<(Entry, Option<usize>)>,
    last_child: Option<usize>,
    space: usize,
    first_node: usize,
) -> (Vec<Node>, Vec<(Entry, Option<usize>)>) {
    let children = last_child.is_some();
    let mut nodes = Vec::new();
    let mut up = Vec::new();
    let mut current: Vec<(Entry, Option<usize>)> = Vec::new();
    for (entry, child) in level {
        let with = node_size(
            current
                .iter()
                .map(|(e, _)| e)
                .chain(std::iter::once(&entry)),
            children,
        );
        if !current.is_empty() && with > space {
            // The entry goes up, and its child closes this node.
            nodes.push(Node {
                entries: std::mem::take(&mut current),
                end_child: child,
            });
            up.push((entry, Some(first_node + nodes.len() - 1)));
        } else {
            current.push((entry, child));
        }
    }
    nodes.push(Node {
        entries: current,
        end_child: last_child,
    });
    (nodes, up)
}

/// Build the index of `entries`, which are in the order of the collation.
pub(crate) fn build(entries: Vec<Entry>, shape: &Shape) -> Index {
    let root_space = shape.root_space.saturating_sub(16 + 16);
    let mut level: Vec<(Entry, Option<usize>)> = entries.into_iter().map(|e| (e, None)).collect();
    let mut last_child: Option<usize> = None;
    let mut blocks: Vec<Node> = Vec::new();
    loop {
        let root = Node {
            entries: level.clone(),
            end_child: last_child,
        };
        if root.size() <= root_space {
            return finish(root, blocks, shape);
        }
        let (nodes, up) = split(level, last_child, shape.block_space(), blocks.len());
        last_child = Some(blocks.len() + nodes.len() - 1);
        blocks.extend(nodes);
        level = up;
    }
}

fn finish(root: Node, nodes: Vec<Node>, shape: &Shape) -> Index {
    let vcn = |n: usize| shape.vcn(n);
    let mut r = vec![0u8; 16 + 16];
    r[0..4].copy_from_slice(&shape.key_kind.to_le_bytes());
    r[4..8].copy_from_slice(&shape.collation.to_le_bytes());
    r[8..12].copy_from_slice(&(shape.block_bytes as u32).to_le_bytes());
    r[12..16].copy_from_slice(&shape.block_field().to_le_bytes());
    let entries = root.entry_bytes(&vcn);
    let total = (ROOT_ENTRIES + entries.len()) as u32;
    r[16..20].copy_from_slice(&(ROOT_ENTRIES as u32).to_le_bytes());
    r[20..24].copy_from_slice(&total.to_le_bytes());
    r[24..28].copy_from_slice(&total.to_le_bytes());
    let flags = if root.has_children() {
        NODE_HAS_CHILDREN
    } else {
        0
    };
    r[28..32].copy_from_slice(&flags.to_le_bytes());
    r.extend_from_slice(&entries);

    let blocks: Vec<Vec<u8>> = nodes
        .iter()
        .enumerate()
        .map(|(n, node)| block_bytes(node, shape.vcn(n), shape, &vcn))
        .collect();
    let mut bitmap = vec![0u8; align8(blocks.len().div_ceil(8)).max(8)];
    for n in 0..blocks.len() {
        bitmap[n / 8] |= 1 << (n % 8);
    }
    if blocks.is_empty() {
        bitmap.clear();
    }
    Index {
        root: r,
        blocks,
        bitmap,
    }
}

fn block_bytes(
    node: &Node,
    vcn_of_block: u64,
    shape: &Shape,
    vcn: &dyn Fn(usize) -> u64,
) -> Vec<u8> {
    let usa_count = 1 + shape.block_bytes / 512;
    let mut b = vec![0u8; shape.block_bytes];
    b[0..4].copy_from_slice(b"INDX");
    b[4..6].copy_from_slice(&(BLOCK_USA_AT as u16).to_le_bytes());
    b[6..8].copy_from_slice(&(usa_count as u16).to_le_bytes());
    b[0x10..0x18].copy_from_slice(&vcn_of_block.to_le_bytes());
    let entries = node.entry_bytes(vcn);
    let total = (BLOCK_ENTRIES + entries.len()) as u32;
    let allocated = (shape.block_bytes - NODE_AT) as u32;
    let n = NODE_AT;
    b[n..n + 4].copy_from_slice(&(BLOCK_ENTRIES as u32).to_le_bytes());
    b[n + 4..n + 8].copy_from_slice(&total.to_le_bytes());
    b[n + 8..n + 12].copy_from_slice(&allocated.to_le_bytes());
    let flags = if node.has_children() {
        NODE_HAS_CHILDREN
    } else {
        0
    };
    b[n + 12..n + 16].copy_from_slice(&flags.to_le_bytes());
    let at = NODE_AT + BLOCK_ENTRIES;
    b[at..at + entries.len()].copy_from_slice(&entries);
    apply_fixups(&mut b, 1);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::record::remove_fixups;

    fn entry(i: u32, key_len: usize) -> Entry {
        let mut key = vec![0u8; key_len];
        key[..4].copy_from_slice(&i.to_be_bytes());
        Entry {
            file: i as u64,
            key,
            data: Vec::new(),
        }
    }

    fn shape(root_space: usize) -> Shape {
        Shape {
            key_kind: 0x30,
            collation: 1,
            block_bytes: 4096,
            cluster_bytes: 4096,
            root_space,
        }
    }

    /// The files of a node and of its children, in the order of the tree,
    /// and the depth of each leaf.
    fn walk(
        index: &Index,
        node: &[u8],
        at: usize,
        depth: usize,
        out: &mut Vec<u64>,
        leaves: &mut Vec<usize>,
    ) {
        let first = u32::from_le_bytes(node[at..at + 4].try_into().unwrap()) as usize;
        let mut e = at + first;
        let has_children = node[at + 12] & 1 != 0;
        if !has_children {
            leaves.push(depth);
        }
        loop {
            let len = u16::from_le_bytes([node[e + 8], node[e + 9]]) as usize;
            let flags = u32::from_le_bytes(node[e + 12..e + 16].try_into().unwrap());
            assert_eq!(
                flags & 1 != 0,
                has_children,
                "each entry of a node with children has one"
            );
            if flags & 1 != 0 {
                let vcn =
                    u64::from_le_bytes(node[e + len - 8..e + len].try_into().unwrap()) as usize;
                let block = remove_fixups(&index.blocks[vcn]).unwrap();
                assert_eq!(&block[0..4], b"INDX");
                assert_eq!(
                    u64::from_le_bytes(block[0x10..0x18].try_into().unwrap()),
                    vcn as u64
                );
                walk(index, &block, NODE_AT, depth + 1, out, leaves);
            }
            if flags & 2 != 0 {
                break;
            }
            out.push(u64::from_le_bytes(node[e..e + 8].try_into().unwrap()));
            e += len;
        }
    }

    fn files(index: &Index) -> (Vec<u64>, Vec<usize>) {
        let (mut out, mut leaves) = (Vec::new(), Vec::new());
        walk(index, &index.root, 16, 0, &mut out, &mut leaves);
        (out, leaves)
    }

    #[test]
    fn a_small_index_lives_in_the_root() {
        let index = build((0..3).map(|i| entry(i, 66)).collect(), &shape(700));
        assert!(index.blocks.is_empty());
        assert!(index.bitmap.is_empty());
        assert_eq!(
            &index.root[0..16],
            &[0x30, 0, 0, 0, 1, 0, 0, 0, 0, 0x10, 0, 0, 1, 0, 0, 0]
        );
        assert_eq!(index.root[28], 0, "a small index");
        assert_eq!(files(&index).0, [0, 1, 2]);
    }

    #[test]
    fn twelve_names_go_into_one_block_as_windows_puts_them() {
        // The root directory of a new volume: the root holds the end entry,
        // and one block holds the 12 names.
        let index = build((0..12).map(|i| entry(i, 80)).collect(), &shape(300));
        assert_eq!(index.blocks.len(), 1);
        assert_eq!(index.bitmap, [1, 0, 0, 0, 0, 0, 0, 0]);
        // The root: header, node with the flag of children, the end entry
        // with the VCN of block 0, as Windows wrote it.
        assert_eq!(
            &index.root[16..32],
            &[0x10, 0, 0, 0, 0x28, 0, 0, 0, 0x28, 0, 0, 0, 1, 0, 0, 0]
        );
        assert_eq!(&index.root[32..], &end_entry(Some(0))[..]);
        let block = remove_fixups(&index.blocks[0]).unwrap();
        assert_eq!(&block[4..8], &[0x28, 0, 9, 0]);
        assert_eq!(&block[0x18..0x1C], &64u32.to_le_bytes());
        assert_eq!(&block[0x20..0x24], &4072u32.to_le_bytes());
        assert_eq!(files(&index).0, (0..12).collect::<Vec<u64>>());
    }

    #[test]
    fn many_entries_make_a_tree_with_each_leaf_at_one_depth() {
        for count in [50u32, 200, 3000, 20000] {
            let index = build((0..count).map(|i| entry(i, 100)).collect(), &shape(400));
            let (got, leaves) = files(&index);
            assert_eq!(
                got,
                (0..count as u64).collect::<Vec<_>>(),
                "{count} entries"
            );
            assert!(
                leaves.iter().all(|d| *d == leaves[0]),
                "{count}: depths {leaves:?}"
            );
            let set = index
                .bitmap
                .iter()
                .map(|b| b.count_ones() as usize)
                .sum::<usize>();
            assert_eq!(set, index.blocks.len());
            for b in &index.blocks {
                let b = remove_fixups(b).unwrap();
                let used = u32::from_le_bytes(b[0x1C..0x20].try_into().unwrap()) as usize;
                assert!(used <= 4096 - 0x18);
            }
        }
    }

    #[test]
    fn a_view_entry_puts_its_data_after_the_key() {
        let e = Entry {
            file: 0,
            key: vec![1, 2, 3, 4],
            data: vec![9; 20],
        };
        let b = e.bytes(None, false);
        assert_eq!(
            &b[0..2],
            &20u16.to_le_bytes(),
            "the data starts after the header and key"
        );
        assert_eq!(&b[2..4], &20u16.to_le_bytes());
        assert_eq!(&b[8..10], &40u16.to_le_bytes());
        assert_eq!(&b[10..12], &4u16.to_le_bytes());
        assert_eq!(&b[20..40], &[9; 20]);
    }
}
