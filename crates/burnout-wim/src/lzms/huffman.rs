//! Canonical Huffman code construction and table-driven decoding.
//!
//! LZMS requires the *exact* code that wimlib's `make_canonical_huffman_code()`
//! produces from a set of frequencies (priority-queue Huffman, ties broken in
//! favour of leaves), so [`make_codeword_lengths`] is a line-by-line port of
//! that function (`sort_symbols`, `build_tree`, `compute_length_counts`, and the
//! length-assignment part of `gen_codewords`). Only the codeword *lengths* are
//! needed by a decoder: the decode table is built directly from them.
//!
//! [`make_decode_table`] ports `make_huffman_decode_table()`. The table format
//! is the same as wimlib's: each `u16` entry is `(symbol << 4) | length`. A
//! root-table entry whose symbol field is `>= 1 << table_bits` is instead a
//! pointer to a subtable: the symbol field is the subtable's start index and
//! the length field is the number of extra bits that index it.

pub const DECODE_TABLE_SYMBOL_SHIFT: u32 = 4;
pub const DECODE_TABLE_LENGTH_MASK: u16 = (1 << DECODE_TABLE_SYMBOL_SHIFT) - 1;

const NUM_SYMBOL_BITS: u32 = 10;
const SYMBOL_MASK: u32 = (1 << NUM_SYMBOL_BITS) - 1;
const FREQ_MASK: u32 = !SYMBOL_MASK;

#[inline]
const fn make_entry(symbol: u32, length: u32) -> u16 {
    ((symbol << DECODE_TABLE_SYMBOL_SHIFT) | length) as u16
}

/// Compute the canonical-Huffman codeword lengths for `freqs`, writing them to
/// `lens`. `work` must hold at least `freqs.len()` entries.
///
/// Requires `2 <= freqs.len() <= 1024` and every frequency `< 2^22` (both always
/// hold in LZMS: alphabets have at most 799 symbols, and frequencies are
/// periodically halved).
pub fn make_codeword_lengths(
    freqs: &[u32],
    max_codeword_len: u32,
    lens: &mut [u8],
    work: &mut [u32],
) {
    let num_syms = freqs.len();
    debug_assert!((2..=1 << NUM_SYMBOL_BITS).contains(&num_syms));
    let a = &mut work[..num_syms];

    // sort_symbols(): wimlib uses a counting sort plus a heap sort of the
    // last bucket; the net result is all symbols with nonzero frequency,
    // sorted by (freq, symbol) ascending, packed as `sym | freq << 10`.
    let mut num_used = 0usize;
    for (sym, &freq) in freqs.iter().enumerate() {
        if freq != 0 {
            a[num_used] = sym as u32 | (freq << NUM_SYMBOL_BITS);
            num_used += 1;
        } else {
            lens[sym] = 0;
        }
    }
    let a = &mut a[..num_used];
    a.sort_unstable(); // keys are unique (symbol in the low bits)

    if num_used == 0 {
        return;
    }
    if num_used == 1 {
        // Degenerate case: give two symbols 1-bit codewords so the code is
        // complete, exactly as wimlib does.
        let sym = (a[0] & SYMBOL_MASK) as usize;
        let nonzero_idx = if sym != 0 { sym } else { 1 };
        lens[0] = 1;
        lens[nonzero_idx] = 1;
        return;
    }

    build_tree(a);

    let mut len_counts = [0u32; 16];
    compute_length_counts(a, num_used - 2, &mut len_counts, max_codeword_len);

    // First half of gen_codewords(): longest codewords go to the
    // least-frequent symbols.
    let mut i = 0usize;
    for len in (1..=max_codeword_len).rev() {
        for _ in 0..len_counts[len as usize] {
            lens[(a[i] & SYMBOL_MASK) as usize] = len as u8;
            i += 1;
        }
    }
}

/// Port of wimlib's `build_tree()`: builds the Huffman tree in place in `a`
/// (two-queue method; leaves win frequency ties). On return, each non-root
/// node's high bits hold its parent's index.
#[allow(clippy::int_plus_one)] // mirrors wimlib exactly
fn build_tree(a: &mut [u32]) {
    let last_idx = a.len() - 1;
    let mut i = 0usize; // next unused leaf
    let mut b = 0usize; // next unused internal node
    let mut e = 0usize; // next internal node slot to fill

    loop {
        let new_freq;
        if i + 1 <= last_idx && (b == e || (a[i + 1] & FREQ_MASK) <= (a[b] & FREQ_MASK)) {
            // Two leaves.
            new_freq = (a[i] & FREQ_MASK) + (a[i + 1] & FREQ_MASK);
            i += 2;
        } else if b + 2 <= e && (i > last_idx || (a[b + 1] & FREQ_MASK) < (a[i] & FREQ_MASK)) {
            // Two internal nodes.
            new_freq = (a[b] & FREQ_MASK) + (a[b + 1] & FREQ_MASK);
            a[b] = ((e as u32) << NUM_SYMBOL_BITS) | (a[b] & SYMBOL_MASK);
            a[b + 1] = ((e as u32) << NUM_SYMBOL_BITS) | (a[b + 1] & SYMBOL_MASK);
            b += 2;
        } else {
            // One leaf and one internal node.
            new_freq = (a[i] & FREQ_MASK) + (a[b] & FREQ_MASK);
            a[b] = ((e as u32) << NUM_SYMBOL_BITS) | (a[b] & SYMBOL_MASK);
            i += 1;
            b += 1;
        }
        a[e] = new_freq | (a[e] & SYMBOL_MASK);
        e += 1;
        if e >= last_idx {
            break;
        }
    }
}

/// Port of wimlib's `compute_length_counts()`, including its length-limiting
/// heuristic (which never triggers for valid LZMS data, but is kept so the
/// result is identical in all cases).
fn compute_length_counts(
    a: &mut [u32],
    root_idx: usize,
    len_counts: &mut [u32; 16],
    max_codeword_len: u32,
) {
    for c in len_counts.iter_mut().take(max_codeword_len as usize + 1) {
        *c = 0;
    }
    len_counts[1] = 2;

    // Root has depth 0.
    a[root_idx] &= SYMBOL_MASK;

    for node in (0..root_idx).rev() {
        let parent = (a[node] >> NUM_SYMBOL_BITS) as usize;
        let parent_depth = a[parent] >> NUM_SYMBOL_BITS;
        let depth = parent_depth + 1;
        let mut len = depth;

        a[node] = (a[node] & SYMBOL_MASK) | (depth << NUM_SYMBOL_BITS);

        if len >= max_codeword_len {
            len = max_codeword_len;
            loop {
                len -= 1;
                if len_counts[len as usize] != 0 {
                    break;
                }
            }
        }
        len_counts[len as usize] -= 1;
        len_counts[len as usize + 1] += 2;
    }
}

/// Build a decode table from codeword lengths. Port of
/// `make_huffman_decode_table()`.
///
/// Returns `false` if the lengths do not form a valid prefix code (this cannot
/// happen for lengths from [`make_codeword_lengths`]). An empty code produces
/// an all-zero table, as in wimlib.
pub fn make_decode_table(
    table: &mut [u16],
    table_bits: u32,
    lens: &[u8],
    max_codeword_len: u32,
) -> bool {
    let num_syms = lens.len();
    let max_len = max_codeword_len as usize;
    let mut len_counts = [0u16; 16];
    let mut offsets = [0u16; 16];
    let mut sorted_syms = [0u16; super::MAX_NUM_SYMS];

    for &l in lens {
        len_counts[l as usize] += 1;
    }

    // Check for an over-subscribed or incomplete code.
    let mut remainder: i32 = 1;
    for &count in &len_counts[1..=max_len] {
        remainder = (remainder << 1) - count as i32;
        if remainder < 0 {
            return false;
        }
    }
    if remainder != 0 {
        if remainder != 1 << max_len {
            return false;
        }
        // Empty code: all lengths 0.
        table[..1 << table_bits].fill(0);
        return true;
    }

    // Sort symbols by (length, symbol).
    offsets[0] = 0;
    for len in 0..max_len {
        offsets[len + 1] = offsets[len] + len_counts[len];
    }
    for (sym, &l) in lens.iter().enumerate() {
        sorted_syms[offsets[l as usize] as usize] = sym as u16;
        offsets[l as usize] += 1;
    }

    // Skip unused symbols (length 0).
    let mut sym_idx = offsets[0] as usize;
    let mut codeword_len: u32 = 1;
    let mut pos = 0usize;

    // Fill entries for codewords short enough to fit in the root table. Each
    // such codeword fills 2^(table_bits - len) consecutive entries, and since
    // the code is canonical these are laid out in order.
    let mut stores_per_loop = 1usize << (table_bits - codeword_len);
    while stores_per_loop != 0 {
        let end_sym_idx = sym_idx + len_counts[codeword_len as usize] as usize;
        while sym_idx < end_sym_idx {
            let v = make_entry(sorted_syms[sym_idx] as u32, codeword_len);
            table[pos..pos + stores_per_loop].fill(v);
            pos += stores_per_loop;
            sym_idx += 1;
        }
        codeword_len += 1;
        stores_per_loop >>= 1;
    }

    if sym_idx == num_syms {
        return true;
    }

    // Long codewords: build subtables.
    let mut codeword: u32 = (pos as u32) << 1;
    let mut subtable_pos: u32 = 1 << table_bits;
    let mut subtable_bits: u32 = table_bits;
    let mut subtable_prefix: u32 = u32::MAX;
    loop {
        while len_counts[codeword_len as usize] == 0 {
            codeword_len += 1;
            codeword <<= 1;
        }

        let prefix = codeword >> (codeword_len - table_bits);

        if prefix != subtable_prefix {
            subtable_prefix = prefix;
            // Size the subtable: start with enough bits for the current
            // codeword length, grow until it can hold all codewords that
            // share this prefix.
            subtable_bits = codeword_len - table_bits;
            let mut rem: i32 = 1 << subtable_bits;
            loop {
                rem -= len_counts[(table_bits + subtable_bits) as usize] as i32;
                if rem <= 0 {
                    break;
                }
                subtable_bits += 1;
                rem <<= 1;
            }
            table[subtable_prefix as usize] = make_entry(subtable_pos, subtable_bits);
        }

        let entry = make_entry(sorted_syms[sym_idx] as u32, codeword_len - table_bits);
        let n = 1usize << (subtable_bits - (codeword_len - table_bits));
        table[subtable_pos as usize..subtable_pos as usize + n].fill(entry);
        subtable_pos += n as u32;

        len_counts[codeword_len as usize] -= 1;
        codeword += 1;
        sym_idx += 1;
        if sym_idx >= num_syms {
            break;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical codeword assignment (second half of wimlib's `gen_codewords`).
    fn codewords_from_lens(lens: &[u8]) -> Vec<u32> {
        let mut counts = [0u32; 17];
        for &l in lens {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut next = [0u32; 17];
        for len in 2..=16 {
            next[len] = (next[len - 1] + counts[len - 1]) << 1;
        }
        lens.iter()
            .map(|&l| {
                let c = next[l as usize];
                next[l as usize] += 1;
                c
            })
            .collect()
    }

    #[test]
    fn uniform_frequencies_give_balanced_code() {
        let freqs = [1u32; 256];
        let mut lens = [0u8; 256];
        let mut work = [0u32; 256];
        make_codeword_lengths(&freqs, 15, &mut lens, &mut work);
        assert!(lens.iter().all(|&l| l == 8));
    }

    #[test]
    fn ties_favour_leaves() {
        // freqs 1,1,2: leaves (1,1) merge -> node 2. Then leaf 2 vs node 2:
        // tie, leaf preferred first, giving lengths [2,2,1].
        let freqs = [1u32, 1, 2];
        let mut lens = [0u8; 3];
        let mut work = [0u32; 3];
        make_codeword_lengths(&freqs, 15, &mut lens, &mut work);
        assert_eq!(lens, [2, 2, 1]);
    }

    #[test]
    fn decode_table_roundtrip_with_subtables() {
        // Skewed frequencies to force long codewords and subtables.
        let mut freqs = vec![1u32; 799];
        for (i, f) in freqs.iter_mut().enumerate().take(20) {
            *f = 1 << (20 - i);
        }
        let mut lens = vec![0u8; 799];
        let mut work = vec![0u32; 799];
        make_codeword_lengths(&freqs, 15, &mut lens, &mut work);
        assert!(lens.iter().any(|&l| l > 11), "want subtables");
        let cws = codewords_from_lens(&lens);
        let table_bits = 11;
        let mut table = vec![0u16; 2854];
        assert!(make_decode_table(&mut table, table_bits, &lens, 15));
        for sym in 0..799 {
            let (cw, len) = (cws[sym], lens[sym] as u32);
            // Left-align the codeword in a 15-bit window and decode.
            let window = cw << (15 - len);
            let mut e = table[(window >> (15 - table_bits)) as usize];
            let mut consumed = 0;
            if (e >> DECODE_TABLE_SYMBOL_SHIFT) as u32 >= 1 << table_bits {
                consumed = table_bits;
                let sub_bits = (e & DECODE_TABLE_LENGTH_MASK) as u32;
                let idx = (window << table_bits & 0x7fff) >> (15 - sub_bits);
                e = table[(e >> DECODE_TABLE_SYMBOL_SHIFT) as usize + idx as usize];
            }
            assert_eq!((e >> DECODE_TABLE_SYMBOL_SHIFT) as usize, sym);
            assert_eq!(consumed + (e & DECODE_TABLE_LENGTH_MASK) as u32, len);
        }
    }
}
