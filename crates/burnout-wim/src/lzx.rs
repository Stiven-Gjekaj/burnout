//! LZX, as a WIM file uses it.
//!
//! Each chunk of a resource is compressed alone, so each call here starts
//! with empty code lengths and the three recent offsets at 1. After the
//! chunk is decoded, the translation of x86 CALL targets is undone, with the
//! file size fixed at 12,000,000 as the WIM format fixes it.
//!
//! The description of the format is public: Microsoft documents LZX in
//! "LZX DELTA Compression and Decompression" and in the Cabinet SDK. This
//! code is new, and follows those descriptions.

use crate::huffman::{Bits, Huffman};

const MIN_MATCH: usize = 2;
const NUM_CHARS: usize = 256;
const LEN_SYMBOLS: usize = 249;
const PRETREE_SYMBOLS: usize = 20;
const ALIGNED_SYMBOLS: usize = 8;
const MAX_OFFSET_SLOTS: usize = 50;
const DEFAULT_BLOCK: usize = 32768;
/// The file size that the WIM format gives the translation of CALL targets.
const E8_FILE_SIZE: i32 = 12_000_000;

const VERBATIM: u32 = 1;
const ALIGNED: u32 = 2;
const UNCOMPRESSED: u32 = 3;

/// The count of offset slots for a window of `1 << order` bytes.
fn offset_slots(order: u32) -> Option<usize> {
    Some(match order {
        15 => 30,
        16 => 32,
        17 => 34,
        18 => 36,
        19 => 38,
        20 => 42,
        21 => 50,
        _ => return None,
    })
}

/// The extra bits and the first offset of each slot.
fn slot_tables() -> ([u32; MAX_OFFSET_SLOTS], [u32; MAX_OFFSET_SLOTS]) {
    let mut extra = [0u32; MAX_OFFSET_SLOTS];
    let mut base = [0u32; MAX_OFFSET_SLOTS];
    for i in 0..MAX_OFFSET_SLOTS {
        extra[i] = if i < 4 {
            0
        } else {
            ((i as u32 - 2) / 2).min(17)
        };
        if i > 0 {
            base[i] = base[i - 1] + (1 << extra[i - 1]);
        }
    }
    (extra, base)
}

/// The window order for chunks of `chunk_bytes`: the smallest power of two
/// that holds a chunk, and at least 2^15.
pub fn window_order(chunk_bytes: u32) -> Option<u32> {
    let order = chunk_bytes
        .max(1)
        .next_power_of_two()
        .trailing_zeros()
        .max(15);
    offset_slots(order).map(|_| order)
}

/// Decompress one chunk into `out`, which has the size of the chunk before
/// compression.
pub fn decompress(input: &[u8], out: &mut [u8], order: u32) -> Result<(), String> {
    let slots = offset_slots(order).ok_or_else(|| format!("an LZX window of 2^{order} bytes"))?;
    let main_symbols = NUM_CHARS + 8 * slots;
    let (extra_bits, slot_base) = slot_tables();
    let mut main_lens = [0u8; NUM_CHARS + 8 * MAX_OFFSET_SLOTS];
    let mut len_lens = [0u8; LEN_SYMBOLS];
    let mut recent = [1usize; 3];
    let mut bits = Bits::new(input);
    let mut pos = 0usize;

    while pos < out.len() {
        let kind = bits.read(3);
        let mut size = if bits.read(1) == 1 {
            DEFAULT_BLOCK
        } else {
            let mut size = bits.read(16) as usize;
            if order >= 16 {
                size = (size << 8) | bits.read(8) as usize;
            }
            size
        };
        if size == 0 {
            return Err("an LZX block of 0 bytes".to_string());
        }
        size = size.min(out.len() - pos);
        let end = pos + size;

        match kind {
            VERBATIM | ALIGNED => {
                let aligned = if kind == ALIGNED {
                    let mut lens = [0u8; ALIGNED_SYMBOLS];
                    for l in &mut lens {
                        *l = bits.read(3) as u8;
                    }
                    Some(Huffman::new(&lens)?)
                } else {
                    None
                };
                read_lengths(&mut bits, &mut main_lens[..NUM_CHARS])?;
                read_lengths(&mut bits, &mut main_lens[NUM_CHARS..main_symbols])?;
                let main = Huffman::new(&main_lens[..main_symbols])?;
                read_lengths(&mut bits, &mut len_lens)?;
                let lengths = Huffman::new(&len_lens)?;

                while pos < end {
                    let symbol = main.decode(&mut bits)? as usize;
                    if symbol < NUM_CHARS {
                        out[pos] = symbol as u8;
                        pos += 1;
                        continue;
                    }
                    let symbol = symbol - NUM_CHARS;
                    let header = symbol & 7;
                    let slot = symbol >> 3;
                    let mut length = header + MIN_MATCH;
                    if header == 7 {
                        length += lengths.decode(&mut bits)? as usize;
                    }
                    let offset = if slot < 3 {
                        recent.swap(0, slot);
                        recent[0]
                    } else {
                        let extra = extra_bits[slot];
                        let formatted = match &aligned {
                            Some(tree) if extra >= 3 => {
                                slot_base[slot]
                                    + (bits.read(extra - 3) << 3)
                                    + tree.decode(&mut bits)?
                            }
                            _ => slot_base[slot] + bits.read(extra),
                        };
                        let offset = formatted as usize - 2;
                        recent = [offset, recent[0], recent[1]];
                        offset
                    };
                    if offset == 0 || offset > pos {
                        return Err(format!(
                            "an LZX match reaches {offset} bytes back from byte {pos}"
                        ));
                    }
                    if length > end - pos {
                        return Err("an LZX match goes past the end of its block".to_string());
                    }
                    for i in pos..pos + length {
                        out[i] = out[i - offset];
                    }
                    pos += length;
                }
            }
            UNCOMPRESSED => {
                bits.align();
                let header = bits
                    .take_bytes(12)
                    .ok_or("an uncompressed LZX block ends in its header")?;
                for (i, r) in recent.iter_mut().enumerate() {
                    let value = u32::from_le_bytes(header[4 * i..4 * i + 4].try_into().unwrap());
                    if value == 0 {
                        return Err(
                            "an uncompressed LZX block gives a recent offset of 0".to_string()
                        );
                    }
                    *r = value as usize;
                }
                let bytes = bits
                    .take_bytes(size)
                    .ok_or("an uncompressed LZX block ends before its size")?;
                out[pos..end].copy_from_slice(bytes);
                pos = end;
                if size % 2 == 1 {
                    // The block keeps the stream on 16-bit words.
                    let _ = bits.take_bytes(1);
                }
            }
            other => return Err(format!("an LZX block of type {other}")),
        }
        if bits.overrun() {
            return Err("the LZX data ends before the chunk does".to_string());
        }
    }
    undo_e8(out);
    Ok(())
}

/// Read code lengths through a pretree. Each length is coded as its change
/// from the length that the previous block gave the same symbol.
fn read_lengths(bits: &mut Bits, lens: &mut [u8]) -> Result<(), String> {
    let mut pre = [0u8; PRETREE_SYMBOLS];
    for l in &mut pre {
        *l = bits.read(4) as u8;
    }
    let pretree = Huffman::new(&pre)?;
    let mut i = 0;
    while i < lens.len() {
        let symbol = pretree.decode(bits)?;
        match symbol {
            0..=16 => {
                lens[i] = ((lens[i] as u32 + 17 - symbol) % 17) as u8;
                i += 1;
            }
            17 | 18 => {
                let run = match symbol {
                    17 => 4 + bits.read(4),
                    _ => 20 + bits.read(5),
                } as usize;
                let stop = (i + run).min(lens.len());
                lens[i..stop].fill(0);
                i = stop;
            }
            _ => {
                let run = 4 + bits.read(1) as usize;
                let next = pretree.decode(bits)?;
                if next > 17 {
                    return Err(format!("a run of code lengths gives the symbol {next}"));
                }
                let value = ((lens[i] as u32 + 17 - next) % 17) as u8;
                let stop = (i + run).min(lens.len());
                lens[i..stop].fill(value);
                i = stop;
            }
        }
    }
    Ok(())
}

/// Undo the translation of the targets of x86 CALL instructions.
fn undo_e8(data: &mut [u8]) {
    if data.len() <= 10 {
        return;
    }
    let tail = data.len() - 10;
    let mut i = 0;
    while i < tail {
        if data[i] != 0xE8 {
            i += 1;
            continue;
        }
        let at = i as i32;
        let target = i32::from_le_bytes(data[i + 1..i + 5].try_into().unwrap());
        let relative = if target >= 0 {
            (target < E8_FILE_SIZE).then(|| target - at)
        } else {
            (target >= -at).then(|| target + E8_FILE_SIZE)
        };
        if let Some(r) = relative {
            data[i + 1..i + 5].copy_from_slice(&r.to_le_bytes());
        }
        i += 5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slots_give_their_extra_bits_and_offsets() {
        let (extra, base) = slot_tables();
        assert_eq!(&extra[..10], &[0, 0, 0, 0, 1, 1, 2, 2, 3, 3]);
        assert_eq!(&base[..10], &[0, 1, 2, 3, 4, 6, 8, 12, 16, 24]);
        assert_eq!((extra[29], base[29]), (13, 24576));
        assert_eq!((extra[36], base[36]), (17, 262144));
        assert_eq!((extra[37], base[37]), (17, 393216));
    }

    #[test]
    fn a_chunk_size_gives_its_window() {
        assert_eq!(window_order(32768), Some(15));
        assert_eq!(window_order(4096), Some(15));
        assert_eq!(window_order(65536), Some(16));
        assert_eq!(window_order(2 * 1024 * 1024), Some(21));
        assert_eq!(window_order(4 * 1024 * 1024), None);
    }

    #[test]
    fn an_uncompressed_block_copies_its_bytes() {
        // Type 3, the default size bit 0, a size of 5 in 16 bits, then the
        // word boundary, three recent offsets, the bytes and one pad byte.
        let mut input = Vec::new();
        let header: u32 = (3 << 29) | (5 << 12);
        input.extend_from_slice(&((header >> 16) as u16).to_le_bytes());
        input.extend_from_slice(&((header & 0xFFFF) as u16).to_le_bytes());
        for r in [1u32, 2, 3] {
            input.extend_from_slice(&r.to_le_bytes());
        }
        input.extend_from_slice(b"hello\0");
        let mut out = [0u8; 5];
        decompress(&input, &mut out, 15).unwrap();
        assert_eq!(&out, b"hello");
    }

    #[test]
    fn a_block_of_an_unknown_type_is_refused() {
        let mut out = [0u8; 4];
        // Type 0, and the bit of the default size.
        let e = decompress(&[0x00, 0x10, 0, 0], &mut out, 15).unwrap_err();
        assert!(e.contains("type 0"), "{e}");
    }

    #[test]
    fn the_translation_of_call_targets_is_undone() {
        let mut data = vec![0u8; 32];
        data[0] = 0xE8;
        data[1..5].copy_from_slice(&105i32.to_le_bytes());
        data[10] = 0xE8;
        data[11..15].copy_from_slice(&(-3i32).to_le_bytes());
        data[20] = 0xE8;
        data[21..25].copy_from_slice(&(E8_FILE_SIZE).to_le_bytes());
        // An E8 in the last 10 bytes stays as it is.
        data[25] = 0xE8;
        data[26..30].copy_from_slice(&7i32.to_le_bytes());
        let mut want = data.clone();
        want[1..5].copy_from_slice(&105i32.to_le_bytes());
        want[11..15].copy_from_slice(&(E8_FILE_SIZE - 3).to_le_bytes());
        undo_e8(&mut data);
        assert_eq!(data, want);

        let mut short = [0xE8, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        undo_e8(&mut short);
        assert_eq!(short[1], 1);
    }
}
