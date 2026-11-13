//! The LZMS x86 machine-code filter (port of wimlib's `lzms_x86_filter()`).
//!
//! The compressor runs this with `undo = false` before compressing, turning
//! relative addresses in likely-x86 instructions into absolute ones; the
//! decompressor runs it with `undo = true` afterwards. It always runs; there is
//! no flag in the compressed data.
//!
//! wimlib writes a sentinel byte near the end of the buffer to avoid a bounds
//! check in the scan loop; here the scan loop is simply bounded, which gives
//! identical results (no translation can start in the last 16 bytes, and a
//! translation starting just before that never reaches the sentinel position).

const X86_ID_WINDOW_SIZE: i32 = 65535;
const X86_MAX_TRANSLATION_OFFSET: i32 = 1023;

#[inline]
fn is_potential_opcode(b: u8) -> bool {
    matches!(b, 0x48 | 0x4C | 0xE8 | 0xE9 | 0xF0 | 0xFF)
}

#[inline]
fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d[p], d[p + 1]])
}

#[inline]
fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
}

/// Run the filter over `data`. `last_target_usages` is scratch space (65536
/// entries) and is fully reinitialised here.
pub fn x86_filter(data: &mut [u8], last_target_usages: &mut [i32; 65536], undo: bool) {
    let size = data.len();
    if size <= 17 {
        return;
    }
    last_target_usages.fill(-X86_ID_WINDOW_SIZE - 1);
    let mut last_x86_pos: i32 = -X86_MAX_TRANSLATION_OFFSET - 1;

    let tail = size - 16;
    // Note: the very first byte is ignored completely.
    let mut p = 1usize;
    loop {
        while p < tail && !is_potential_opcode(data[p]) {
            p += 1;
        }
        if p >= tail {
            break;
        }
        p = translate_if_needed(data, p, &mut last_x86_pos, last_target_usages, undo);
    }
}

/// Examine the potential opcode at `data[p]` and translate the following
/// 32-bit displacement if appropriate. Returns the position to resume
/// scanning from.
#[inline]
fn translate_if_needed(
    data: &mut [u8],
    mut p: usize,
    last_x86_pos: &mut i32,
    last_target_usages: &mut [i32; 65536],
    undo: bool,
) -> usize {
    let mut max_trans_offset = X86_MAX_TRANSLATION_OFFSET;
    let b0 = data[p];

    let opcode_nbytes: usize = if b0 >= 0xF0 {
        if b0 & 0x0F != 0 {
            // 0xFF: call indirect relative (FF 15 disp32)
            if data[p + 1] == 0x15 {
                2
            } else {
                return p + 1;
            }
        } else {
            // 0xF0: lock add relative (F0 83 05 disp32 imm8)
            if data[p + 1] == 0x83 && data[p + 2] == 0x05 {
                3
            } else {
                return p + 1;
            }
        }
    } else if b0 <= 0x4C {
        // 0x48 / 0x4C REX.W prefix followed by LEA or MOV with what is meant
        // to be a RIP-relative ModR/M byte. (The format only tests the R/M
        // field, mask 0x07, rather than 0xC7; this quirk must be preserved.)
        let (b1, b2) = (data[p + 1], data[p + 2]);
        if (b2 & 0x07) == 0x05
            && (b1 == 0x8D || (b1 == 0x8B && (b0 & 0x04) == 0 && (b2 & 0xF0) == 0))
        {
            3
        } else {
            return p + 1;
        }
    } else if b0 & 0x01 != 0 {
        // 0xE9: jump relative. Deliberately *not* translated, but its
        // displacement is skipped.
        return p + 5;
    } else {
        // 0xE8: call relative. Common, so require stronger evidence of x86
        // code before translating.
        max_trans_offset >>= 1;
        1
    };

    // have_opcode:
    let mut i = p as i32;
    p += opcode_nbytes;
    let target16: u16;
    if undo {
        if i - *last_x86_pos <= max_trans_offset {
            let n = le32(data, p);
            data[p..p + 4].copy_from_slice(&n.wrapping_sub(i as u32).to_le_bytes());
        }
        target16 = (i as u16).wrapping_add(le16(data, p));
    } else {
        target16 = (i as u16).wrapping_add(le16(data, p));
        if i - *last_x86_pos <= max_trans_offset {
            let n = le32(data, p);
            data[p..p + 4].copy_from_slice(&n.wrapping_add(i as u32).to_le_bytes());
        }
    }

    i += (opcode_nbytes + 4 - 1) as i32;
    if i - last_target_usages[target16 as usize] <= X86_ID_WINDOW_SIZE {
        *last_x86_pos = i;
    }
    last_target_usages[target16 as usize] = i;

    p + 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_roundtrips_on_x86_like_data() {
        // Many E8 calls to the same targets so the detector kicks in.
        let mut data = Vec::new();
        for k in 0..4000u32 {
            data.extend_from_slice(&[0x90, 0xE8]);
            data.extend_from_slice(&(0x1000u32.wrapping_sub(k * 6)).to_le_bytes());
            if k % 7 == 0 {
                data.extend_from_slice(&[0x48, 0x8D, 0x05, 0x10, 0x20, 0x00, 0x00]);
            }
        }
        let orig = data.clone();
        let mut ltu = Box::new([0i32; 65536]);
        x86_filter(&mut data, &mut ltu, false);
        assert_ne!(data, orig, "filter should have translated something");
        x86_filter(&mut data, &mut ltu, true);
        assert_eq!(data, orig);
    }
}
