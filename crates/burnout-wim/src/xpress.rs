//! XPRESS with Huffman codes, as a WIM file uses it.
//!
//! Microsoft documents the format in [MS-XCA], "LZ77+Huffman". This code is
//! new, and follows the decompression steps of that document, with the same
//! place in the input for each read. That place matters, because the extra
//! bytes of a long match come from the input between the words of bits.

use crate::huffman::Huffman;

const SYMBOLS: usize = 512;
const TABLE_BYTES: usize = SYMBOLS / 2;
const BLOCK: usize = 65536;
const MIN_MATCH: usize = 3;

/// The reader of [MS-XCA]: 32 bits, and at least 16 of them not read.
struct Input<'a> {
    data: &'a [u8],
    pos: usize,
    next: u32,
    /// The bits held past the first 16.
    extra: i32,
}

impl<'a> Input<'a> {
    fn u16_at(&self, at: usize) -> u32 {
        match self.data.get(at..at + 2) {
            Some(w) => u16::from_le_bytes([w[0], w[1]]) as u32,
            None => 0,
        }
    }

    fn start(data: &'a [u8], pos: usize) -> Input<'a> {
        let mut input = Input {
            data,
            pos,
            next: 0,
            extra: 16,
        };
        input.next = (input.u16_at(pos) << 16) | input.u16_at(pos + 2);
        input.pos += 4;
        input
    }

    fn consume(&mut self, n: u32) {
        self.next = self.next.checked_shl(n).unwrap_or(0);
        self.extra -= n as i32;
        if self.extra < 0 {
            self.next |= self.u16_at(self.pos) << (-self.extra);
            self.extra += 16;
            self.pos += 2;
        }
    }

    fn byte(&mut self) -> Result<u32, String> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or("the XPRESS data ends in the length of a match")?;
        self.pos += 1;
        Ok(b as u32)
    }

    fn word(&mut self) -> Result<u32, String> {
        if self.pos + 2 > self.data.len() {
            return Err("the XPRESS data ends in the length of a match".to_string());
        }
        let w = self.u16_at(self.pos);
        self.pos += 2;
        Ok(w)
    }
}

/// Decompress one chunk into `out`, which has the size of the chunk before
/// compression.
pub fn decompress(input: &[u8], out: &mut [u8]) -> Result<(), String> {
    let mut pos = 0usize;
    let mut at = 0usize;
    while pos < out.len() {
        let table = input
            .get(at..at + TABLE_BYTES)
            .ok_or("the XPRESS data ends in its table of code lengths")?;
        let mut lens = [0u8; SYMBOLS];
        for (i, b) in table.iter().enumerate() {
            lens[2 * i] = b & 0x0F;
            lens[2 * i + 1] = b >> 4;
        }
        let code = Huffman::new(&lens)?;
        let mut bits = Input::start(input, at + TABLE_BYTES);
        let end = (pos + BLOCK).min(out.len());
        while pos < end {
            let (symbol, len) = code.lookup(bits.next >> 16)?;
            bits.consume(len);
            if symbol < 256 {
                out[pos] = symbol as u8;
                pos += 1;
                continue;
            }
            let symbol = symbol - 256;
            let mut length = (symbol & 15) as usize;
            let offset_bits = symbol >> 4;
            if length == 15 {
                length = bits.byte()? as usize;
                if length == 255 {
                    length = bits.word()? as usize;
                    if length < 15 {
                        return Err("an XPRESS match gives a length below 15".to_string());
                    }
                    length -= 15;
                }
                length += 15;
            }
            length += MIN_MATCH;
            let offset = match offset_bits {
                0 => 1,
                n => ((bits.next >> (32 - n)) + (1 << n)) as usize,
            };
            bits.consume(offset_bits);
            if offset > pos {
                return Err(format!(
                    "an XPRESS match reaches {offset} bytes back from byte {pos}"
                ));
            }
            if length > out.len() - pos {
                return Err("an XPRESS match goes past the end of the chunk".to_string());
            }
            for i in pos..pos + length {
                out[i] = out[i - offset];
            }
            pos += length;
        }
        at = bits.pos;
        if at > input.len() + 4 {
            return Err("the XPRESS data ends before the chunk does".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_come_through() {
        // Each of the 256 literals has a code of 8 bits, and the rest none.
        // The code of a byte is the byte itself.
        let mut input = vec![0x88u8; 128];
        input.extend_from_slice(&[0; 128]);
        input.extend_from_slice(&[b'i', b'h', 0, b'!']);
        let mut out = [0u8; 3];
        decompress(&input, &mut out).unwrap();
        assert_eq!(&out, b"hi!");
    }

    #[test]
    fn a_short_table_is_refused() {
        let mut out = [0u8; 3];
        let e = decompress(&[0x88; 100], &mut out).unwrap_err();
        assert!(e.contains("table of code lengths"), "{e}");
    }
}
