//! A registry hive, which a BCD store is.
//!
//! The format is the "regf" format of Windows: a base block of 4 KiB, and
//! bins of 4 KiB that hold cells. A key is an "nk" cell with a list of its
//! subkeys ("lh", with a hash of each name), a list of its values, and a
//! security cell ("sk") that each key of this hive shares. A value is a "vk"
//! cell, with its data in a cell of its own, or in the cell itself when it
//! has 4 bytes or fewer. The layout follows the descriptions of the format
//! that the community published, and the BCD stores of Windows 11 25H2 that
//! the tests read. It is new code.

use std::collections::BTreeMap;

pub const REG_SZ: u32 = 1;
pub const REG_BINARY: u32 = 3;
pub const REG_DWORD: u32 = 4;
pub const REG_MULTI_SZ: u32 = 7;

const BIN: usize = 4096;
const BIN_HEADER: usize = 32;
/// The largest value that this writer holds in one cell, which fits one bin
/// of 4 KiB. A BCD store has no larger value.
const MAX_INLINE_DATA: usize = BIN - BIN_HEADER - 8;

/// One value of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub kind: u32,
    pub data: Vec<u8>,
}

impl Value {
    pub fn string(s: &str) -> Value {
        let mut data: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        data.extend_from_slice(&[0, 0]);
        Value { kind: REG_SZ, data }
    }

    pub fn strings(items: &[&str]) -> Value {
        let mut data = Vec::new();
        for s in items {
            data.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
            data.extend_from_slice(&[0, 0]);
        }
        data.extend_from_slice(&[0, 0]);
        Value {
            kind: REG_MULTI_SZ,
            data,
        }
    }

    pub fn dword(v: u32) -> Value {
        Value {
            kind: REG_DWORD,
            data: v.to_le_bytes().to_vec(),
        }
    }

    pub fn binary(data: &[u8]) -> Value {
        Value {
            kind: REG_BINARY,
            data: data.to_vec(),
        }
    }
}

/// A key: its values by name, and its subkeys by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub values: BTreeMap<String, Value>,
    pub keys: BTreeMap<String, Key>,
}

impl Key {
    /// The subkey at `path`, a list of names, made when it is not there.
    pub fn key(&mut self, path: &[&str]) -> &mut Key {
        let mut at = self;
        for name in path {
            at = at.keys.entry((*name).to_string()).or_default();
        }
        at
    }

    pub fn set(&mut self, name: &str, value: Value) {
        self.values.insert(name.to_string(), value);
    }
}

/// The hash of a name in an "lh" list: for each character in upper case,
/// multiply by 37 and add.
fn name_hash(name: &str) -> u32 {
    name.chars()
        .flat_map(char::to_uppercase)
        .fold(0u32, |h, c| h.wrapping_mul(37).wrapping_add(c as u32))
}

/// The order of subkeys in a list: by the name in upper case.
fn upper(name: &str) -> Vec<u16> {
    name.to_uppercase().encode_utf16().collect()
}

/// A name as the hive stores it: ASCII in one byte a character, with the
/// flag that says so, or UTF-16.
fn stored_name(name: &str) -> (Vec<u8>, bool) {
    if name.is_ascii() {
        (name.as_bytes().to_vec(), true)
    } else {
        (
            name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
            false,
        )
    }
}

/// The cells of the hive, placed one after another in bins.
struct Cells {
    data: Vec<u8>,
}

impl Cells {
    /// Place a cell with these bytes, and give its offset from the first
    /// bin. A cell does not cross the end of a bin.
    fn put(&mut self, body: &[u8]) -> u32 {
        let size = (4 + body.len() + 7) & !7;
        assert!(
            size <= BIN - BIN_HEADER,
            "a cell of {size} bytes does not fit a bin"
        );
        let in_bin = self.data.len() % BIN;
        if in_bin == 0 || in_bin + size > BIN {
            self.close_bin();
            self.open_bin();
        }
        let at = self.data.len();
        self.data.extend_from_slice(&(-(size as i32)).to_le_bytes());
        self.data.extend_from_slice(body);
        self.data.resize(at + size, 0);
        at as u32
    }

    fn open_bin(&mut self) {
        let offset = self.data.len() as u32;
        let mut h = vec![0u8; BIN_HEADER];
        h[0..4].copy_from_slice(b"hbin");
        h[4..8].copy_from_slice(&offset.to_le_bytes());
        h[8..12].copy_from_slice(&(BIN as u32).to_le_bytes());
        self.data.extend_from_slice(&h);
    }

    /// Fill the rest of the bin with one free cell.
    fn close_bin(&mut self) {
        let rest = (BIN - self.data.len() % BIN) % BIN;
        if rest > 0 {
            let at = self.data.len();
            self.data.resize(at + rest, 0);
            self.data[at..at + 4].copy_from_slice(&(rest as i32).to_le_bytes());
        }
    }

    /// Change four bytes of a placed cell.
    fn patch(&mut self, at: u32, offset: usize, value: u32) {
        let p = at as usize + 4 + offset;
        self.data[p..p + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// The security descriptor that each key of the hive shares: the one of the
/// BCD stores of Windows, which allows the administrators and SYSTEM full
/// control and others read.
fn security_cell(descriptor: &[u8], keys: u32) -> Vec<u8> {
    let mut b = vec![0u8; 20];
    b[0..2].copy_from_slice(b"sk");
    // The list of security cells holds this one only, so it links to
    // itself: the two links are set when the offset is known.
    b[12..16].copy_from_slice(&keys.to_le_bytes());
    b[16..20].copy_from_slice(&(descriptor.len() as u32).to_le_bytes());
    b.extend_from_slice(descriptor);
    b
}

/// Write `key` with its tree as a hive. `name` goes into the base block,
/// as Windows writes the path of the file there.
pub fn write_hive(root: &Key, name: &str, descriptor: &[u8], time: u64) -> Result<Vec<u8>, String> {
    let mut cells = Cells { data: Vec::new() };
    let count = count_keys(root);
    let sk = cells.put(&security_cell(descriptor, count));
    cells.patch(sk, 4, sk);
    cells.patch(sk, 8, sk);
    let root_at = put_key(
        &mut cells,
        root,
        "NewStoreRoot",
        0xFFFF_FFFF,
        sk,
        time,
        true,
    )?;
    cells.close_bin();
    let bins = cells.data;

    let mut base = vec![0u8; 4096];
    base[0..4].copy_from_slice(b"regf");
    base[4..8].copy_from_slice(&1u32.to_le_bytes());
    base[8..12].copy_from_slice(&1u32.to_le_bytes());
    base[12..20].copy_from_slice(&time.to_le_bytes());
    base[20..24].copy_from_slice(&1u32.to_le_bytes());
    base[24..28].copy_from_slice(&3u32.to_le_bytes());
    base[28..32].copy_from_slice(&0u32.to_le_bytes());
    base[32..36].copy_from_slice(&1u32.to_le_bytes());
    base[36..40].copy_from_slice(&root_at.to_le_bytes());
    base[40..44].copy_from_slice(&(bins.len() as u32).to_le_bytes());
    base[44..48].copy_from_slice(&1u32.to_le_bytes());
    let file: Vec<u8> = name
        .encode_utf16()
        .take(31)
        .flat_map(|u| u.to_le_bytes())
        .collect();
    base[48..48 + file.len()].copy_from_slice(&file);
    let checksum = base[..508]
        .chunks_exact(4)
        .fold(0u32, |x, w| x ^ u32::from_le_bytes(w.try_into().unwrap()));
    base[508..512].copy_from_slice(&checksum.to_le_bytes());
    let mut out = base;
    out.extend_from_slice(&bins);
    Ok(out)
}

fn count_keys(key: &Key) -> u32 {
    1 + key.keys.values().map(count_keys).sum::<u32>()
}

/// Place `key` and what it holds, and give the offset of its "nk" cell.
fn put_key(
    cells: &mut Cells,
    key: &Key,
    name: &str,
    parent: u32,
    sk: u32,
    time: u64,
    root: bool,
) -> Result<u32, String> {
    let (stored, ascii) = stored_name(name);
    let mut nk = vec![0u8; 0x4C];
    nk[0..2].copy_from_slice(b"nk");
    let flags: u16 = (if ascii { 0x20 } else { 0 }) | if root { 0x0C } else { 0 };
    nk[2..4].copy_from_slice(&flags.to_le_bytes());
    nk[4..12].copy_from_slice(&time.to_le_bytes());
    nk[0x10..0x14].copy_from_slice(&parent.to_le_bytes());
    nk[0x14..0x18].copy_from_slice(&(key.keys.len() as u32).to_le_bytes());
    nk[0x1C..0x20].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    nk[0x20..0x24].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    nk[0x24..0x28].copy_from_slice(&(key.values.len() as u32).to_le_bytes());
    nk[0x28..0x2C].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    nk[0x2C..0x30].copy_from_slice(&sk.to_le_bytes());
    nk[0x30..0x34].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let longest_key = key
        .keys
        .keys()
        .map(|k| 2 * k.encode_utf16().count())
        .max()
        .unwrap_or(0);
    let longest_value = key
        .values
        .keys()
        .map(|k| 2 * k.encode_utf16().count())
        .max()
        .unwrap_or(0);
    let largest_data = key.values.values().map(|v| v.data.len()).max().unwrap_or(0);
    nk[0x34..0x38].copy_from_slice(&(longest_key as u32).to_le_bytes());
    nk[0x3C..0x40].copy_from_slice(&(longest_value as u32).to_le_bytes());
    nk[0x40..0x44].copy_from_slice(&(largest_data as u32).to_le_bytes());
    nk[0x48..0x4A].copy_from_slice(&(stored.len() as u16).to_le_bytes());
    nk.extend_from_slice(&stored);
    let at = cells.put(&nk);

    if !key.keys.is_empty() {
        let mut subkeys: Vec<(&String, &Key)> = key.keys.iter().collect();
        subkeys.sort_by_key(|(n, _)| upper(n));
        let mut list = Vec::with_capacity(4 + 8 * subkeys.len());
        list.extend_from_slice(b"lh");
        list.extend_from_slice(&(subkeys.len() as u16).to_le_bytes());
        let mut offsets = Vec::new();
        for (n, k) in &subkeys {
            offsets.push((put_key(cells, k, n, at, sk, time, false)?, name_hash(n)));
        }
        for (o, h) in offsets {
            list.extend_from_slice(&o.to_le_bytes());
            list.extend_from_slice(&h.to_le_bytes());
        }
        let list_at = cells.put(&list);
        cells.patch(at, 0x1C, list_at);
    }

    if !key.values.is_empty() {
        let mut offsets = Vec::new();
        for (n, v) in &key.values {
            if v.data.len() > MAX_INLINE_DATA {
                return Err(format!(
                    "the value {n} holds {} bytes, more than one cell",
                    v.data.len()
                ));
            }
            let (vname, vascii) = stored_name(n);
            let mut vk = vec![0u8; 0x14];
            vk[0..2].copy_from_slice(b"vk");
            vk[2..4].copy_from_slice(&(vname.len() as u16).to_le_bytes());
            if v.data.len() <= 4 {
                let mut inline = [0u8; 4];
                inline[..v.data.len()].copy_from_slice(&v.data);
                vk[4..8].copy_from_slice(&(v.data.len() as u32 | 0x8000_0000).to_le_bytes());
                vk[8..12].copy_from_slice(&inline);
            } else {
                let data_at = cells.put(&v.data);
                vk[4..8].copy_from_slice(&(v.data.len() as u32).to_le_bytes());
                vk[8..12].copy_from_slice(&data_at.to_le_bytes());
            }
            vk[12..16].copy_from_slice(&v.kind.to_le_bytes());
            vk[16..18].copy_from_slice(&(vascii as u16).to_le_bytes());
            vk.extend_from_slice(&vname);
            offsets.push(cells.put(&vk));
        }
        let list: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
        let list_at = cells.put(&list);
        cells.patch(at, 0x28, list_at);
    }
    Ok(at)
}

/// A reader of hives, for the tests and for a check of a store.
pub mod read {
    use super::*;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// The tree of a hive, with the name of its root key.
    pub fn read_hive(hive: &[u8]) -> Result<(String, Key), String> {
        if hive.len() < 4096 || &hive[0..4] != b"regf" {
            return Err("not a hive".to_string());
        }
        let bins = &hive[4096..];
        let root = u32_at(hive, 36);
        read_key(bins, root, 0)
    }

    fn cell(bins: &[u8], at: u32) -> Result<&[u8], String> {
        let at = at as usize;
        let size = i32::from_le_bytes(
            bins.get(at..at + 4)
                .ok_or("a cell past the end")?
                .try_into()
                .unwrap(),
        );
        if size >= 0 {
            return Err(format!("the cell at {at} is free"));
        }
        bins.get(at + 4..at + (-size) as usize)
            .ok_or_else(|| format!("the cell at {at} ends past the end"))
    }

    fn name_of(b: &[u8], len: usize, ascii: bool) -> String {
        if ascii {
            b[..len].iter().map(|c| *c as char).collect()
        } else {
            let u: Vec<u16> = b[..len]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&u)
        }
    }

    fn read_key(bins: &[u8], at: u32, depth: usize) -> Result<(String, Key), String> {
        if depth > 64 {
            return Err("the tree is too deep".to_string());
        }
        let nk = cell(bins, at)?;
        if &nk[0..2] != b"nk" {
            return Err(format!("the cell at {at} is not a key"));
        }
        let flags = u16_at(nk, 2);
        let name = name_of(&nk[0x4C..], u16_at(nk, 0x48) as usize, flags & 0x20 != 0);
        let mut key = Key::default();
        let subkeys = u32_at(nk, 0x14);
        if subkeys > 0 {
            let list = cell(bins, u32_at(nk, 0x1C))?;
            let step = if &list[0..2] == b"li" { 4 } else { 8 };
            for i in 0..u16_at(list, 2) as usize {
                let (n, k) = read_key(bins, u32_at(list, 4 + step * i), depth + 1)?;
                key.keys.insert(n, k);
            }
        }
        let values = u32_at(nk, 0x24);
        if values > 0 {
            let list = cell(bins, u32_at(nk, 0x28))?;
            for i in 0..values as usize {
                let vk = cell(bins, u32_at(list, 4 * i))?;
                let vname = name_of(
                    &vk[0x14..],
                    u16_at(vk, 2) as usize,
                    u16_at(vk, 0x10) & 1 != 0,
                );
                let size = u32_at(vk, 4);
                let data = if size & 0x8000_0000 != 0 {
                    vk[8..8 + (size & 0x7FFF_FFFF) as usize].to_vec()
                } else {
                    cell(bins, u32_at(vk, 8))?[..size as usize].to_vec()
                };
                key.values.insert(
                    vname,
                    Value {
                        kind: u32_at(vk, 12),
                        data,
                    },
                );
            }
        }
        Ok((name, key))
    }
}

#[cfg(test)]
mod tests {
    use super::read::read_hive;
    use super::*;

    #[test]
    fn a_tree_comes_back_from_its_hive() {
        let mut root = Key::default();
        root.key(&["Description"])
            .set("KeyName", Value::string("BCD00000000"));
        root.key(&["Description"]).set("System", Value::dword(1));
        let o = root.key(&["Objects", "{9dea862c-5cdd-4e70-acc1-f32b344d4795}"]);
        o.key(&["Description"])
            .set("Type", Value::dword(0x1010_0002));
        o.key(&["Elements", "12000004"])
            .set("Element", Value::string("Windows Boot Manager"));
        o.key(&["Elements", "24000001"])
            .set("Element", Value::strings(&["{a}", "{b}"]));
        o.key(&["Elements", "25000004"])
            .set("Element", Value::binary(&[30, 0, 0, 0, 0, 0, 0, 0]));
        for i in 0..300 {
            root.key(&["Objects", &format!("{{many-{i:03}}}")])
                .set("x", Value::binary(&[1; 40]));
        }
        let hive = write_hive(&root, "\\BCD", &[1, 0, 4, 0x80], 1).unwrap();
        assert_eq!(&hive[0..4], b"regf");
        assert_eq!(hive.len() % 4096, 0);
        let (name, back) = read_hive(&hive).unwrap();
        assert_eq!(name, "NewStoreRoot");
        assert_eq!(back, root);
    }

    #[test]
    fn the_base_block_has_its_checksum_and_each_bin_its_header() {
        let mut root = Key::default();
        root.set("a", Value::dword(7));
        let hive = write_hive(&root, "x", &[1, 0, 4, 0x80], 5).unwrap();
        let xor = hive[..508]
            .chunks_exact(4)
            .fold(0u32, |x, w| x ^ u32::from_le_bytes(w.try_into().unwrap()));
        assert_eq!(&hive[508..512], &xor.to_le_bytes());
        for (i, bin) in hive[4096..].chunks(4096).enumerate() {
            assert_eq!(&bin[0..4], b"hbin");
            assert_eq!(&bin[4..8], &((i * 4096) as u32).to_le_bytes());
        }
    }

    #[test]
    fn a_name_hashes_as_the_lh_list_needs() {
        assert_eq!(name_hash("a"), 'A' as u32);
        assert_eq!(name_hash("ab"), 'A' as u32 * 37 + 'B' as u32);
        assert_eq!(name_hash("Objects"), name_hash("OBJECTS"));
    }

    #[test]
    fn the_store_of_windows_reads() {
        // The BCD store on the Windows 11 25H2 Arm64 ISO, when it is at hand.
        let Ok(path) = std::env::var("BCD_SAMPLE") else {
            return;
        };
        let (_, tree) = read_hive(&std::fs::read(path).unwrap()).unwrap();
        assert!(tree.keys["Objects"].keys.len() > 5);
    }
}
