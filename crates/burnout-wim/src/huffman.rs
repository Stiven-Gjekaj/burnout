//! Canonical Huffman codes, and the bit reader of LZX.
//!
//! LZX reads its bits from 16-bit little-endian words, the high bit first.
//! A code is canonical: the lengths alone give each code, shorter codes come
//! first, and codes of the same length go in the order of their symbols.

/// The longest code that LZX uses.
pub const MAX_LEN: usize = 16;

/// The bits of a short code that the first table looks up at once.
const FAST_BITS: u32 = 10;

/// Bits read from 16-bit little-endian words, the high bit first.
pub struct Bits<'a> {
    data: &'a [u8],
    /// The byte of the next word.
    pos: usize,
    /// The bits not yet read, at the top of the word.
    buf: u32,
    left: u32,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Bits<'a> {
        Bits {
            data,
            pos: 0,
            buf: 0,
            left: 0,
        }
    }

    /// Hold at least `n` bits, `n` at most 16. Past the end of the data, the
    /// words read as zero, and `overrun` says so after the end.
    fn ensure(&mut self, n: u32) {
        while self.left < n {
            let word = match self.data.get(self.pos..self.pos + 2) {
                Some(w) => u16::from_le_bytes([w[0], w[1]]) as u32,
                None => 0,
            };
            self.pos += 2;
            self.buf |= word << (16 - self.left);
            self.left += 16;
        }
    }

    /// The next 16 bits, and no bit is read.
    pub fn peek16(&mut self) -> u32 {
        self.ensure(16);
        self.buf >> 16
    }

    pub fn consume(&mut self, n: u32) {
        debug_assert!(n <= self.left);
        self.buf = self.buf.checked_shl(n).unwrap_or(0);
        self.left -= n;
    }

    /// Read `n` bits, `n` at most 32.
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if n > 16 {
            let high = self.read(n - 16);
            return (high << 16) | self.read(16);
        }
        self.ensure(n);
        let value = self.buf >> (32 - n);
        self.consume(n);
        value
    }

    /// Go to the next word, as an uncompressed block of LZX needs. The place
    /// counts the bits that were read, and not the bits that the reader holds
    /// ahead. A place that is on a word already skips a whole word.
    pub fn align(&mut self) {
        let part = self.left % 16;
        let skip = if part == 0 { 16 } else { part };
        if self.left >= skip {
            self.left -= skip;
        } else {
            // The reader holds no bit, so the word to skip is the next one.
            self.pos += 2;
        }
        // The whole words that the reader holds ahead are not read yet.
        self.pos -= (self.left / 16) as usize * 2;
        self.buf = 0;
        self.left = 0;
    }

    /// The bytes from the next word on, read past the bit reader. `align`
    /// comes first.
    pub fn take_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        debug_assert_eq!(self.left, 0);
        let bytes = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(bytes)
    }

    /// The reader read words past the end of the data.
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() + 4
    }
}

/// A Huffman code, from the length of the code of each symbol.
#[derive(Debug)]
pub struct Huffman {
    /// For each value of the first `FAST_BITS` bits: the symbol and its
    /// length, or 0 for a longer code.
    fast: Vec<u32>,
    /// For each length: the first code, and the count of codes.
    first: [u32; MAX_LEN + 1],
    count: [u32; MAX_LEN + 1],
    /// Where the symbols of each length start in `sorted`.
    start: [u32; MAX_LEN + 1],
    sorted: Vec<u16>,
}

impl Huffman {
    /// The code for these lengths. A length of 0 means that the symbol has
    /// no code. Too many short codes is an error. Too few is allowed, and a
    /// code that is not given fails when it is read.
    pub fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let mut count = [0u32; MAX_LEN + 1];
        for &l in lengths {
            if l as usize > MAX_LEN {
                return Err(format!("a code of {l} bits, longer than {MAX_LEN}"));
            }
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut space: i64 = 1;
        for &c in &count[1..] {
            space = space * 2 - c as i64;
            if space < 0 {
                return Err("the lengths of a code give more codes than fit".to_string());
            }
        }
        let mut first = [0u32; MAX_LEN + 1];
        let mut start = [0u32; MAX_LEN + 1];
        let (mut code, mut at) = (0u32, 0u32);
        for len in 1..=MAX_LEN {
            code = (code + count[len - 1]) << 1;
            first[len] = code;
            start[len] = at;
            at += count[len];
        }
        let mut sorted = vec![0u16; at as usize];
        let mut next = start;
        for (symbol, &l) in lengths.iter().enumerate() {
            if l > 0 {
                sorted[next[l as usize] as usize] = symbol as u16;
                next[l as usize] += 1;
            }
        }
        let mut fast = vec![0u32; 1 << FAST_BITS];
        for len in 1..=FAST_BITS as usize {
            for i in 0..count[len] {
                let symbol = sorted[(start[len] + i) as usize] as u32;
                let code = first[len] + i;
                let shift = FAST_BITS as usize - len;
                let base = (code << shift) as usize;
                for slot in &mut fast[base..base + (1 << shift)] {
                    *slot = (symbol << 5) | len as u32;
                }
            }
        }
        Ok(Huffman {
            fast,
            first,
            count,
            start,
            sorted,
        })
    }

    /// Read one symbol of LZX.
    pub fn decode(&self, bits: &mut Bits) -> Result<u32, String> {
        let (symbol, len) = self.lookup(bits.peek16())?;
        bits.consume(len);
        Ok(symbol)
    }

    /// The symbol whose code starts the 16 bits of `peek`, the first bit at
    /// the top, and the length of its code.
    pub fn lookup(&self, peek: u32) -> Result<(u32, u32), String> {
        let entry = self.fast[(peek >> (16 - FAST_BITS)) as usize];
        if entry != 0 {
            return Ok((entry >> 5, entry & 31));
        }
        for len in FAST_BITS as usize + 1..=MAX_LEN {
            let code = peek >> (16 - len);
            let index = code.wrapping_sub(self.first[len]);
            if index < self.count[len] {
                let symbol = self.sorted[(self.start[len] + index) as usize] as u32;
                return Ok((symbol, len as u32));
            }
        }
        Err("the data holds a code that the Huffman table does not give".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bits, the high bit first, packed into 16-bit little-endian words.
    fn pack(bits: &str) -> Vec<u8> {
        let bits: Vec<u8> = bits
            .bytes()
            .filter(|b| *b != b' ')
            .map(|b| b - b'0')
            .collect();
        let mut out = Vec::new();
        for word in bits.chunks(16) {
            let mut w = 0u16;
            for (i, b) in word.iter().enumerate() {
                w |= (*b as u16) << (15 - i);
            }
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    #[test]
    fn bits_come_from_words_high_bit_first() {
        let data = [0x34, 0x12, 0x78, 0x56];
        let mut b = Bits::new(&data);
        assert_eq!(b.read(4), 0x1);
        assert_eq!(b.read(8), 0x23);
        assert_eq!(b.read(8), 0x45);
        assert_eq!(b.read(12), 0x678);
        assert!(!b.overrun());
        let mut b = Bits::new(&data);
        assert_eq!(b.read(17), 0x1234 << 1);
        assert_eq!(b.read(15), 0x5678 & 0x7FFF);
    }

    #[test]
    fn align_skips_to_the_next_word_and_a_whole_word_when_aligned() {
        let data = [0xFF, 0xFF, 1, 2, 3, 4];
        let mut b = Bits::new(&data);
        b.read(3);
        b.align();
        assert_eq!(b.take_bytes(2), Some(&[1u8, 2][..]));
        let mut b = Bits::new(&data);
        b.align();
        assert_eq!(b.take_bytes(4), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(b.take_bytes(1), None);
        // 16 bits read are on a word, so the next word is skipped.
        let mut b = Bits::new(&data);
        b.read(16);
        b.align();
        assert_eq!(b.take_bytes(2), Some(&[3u8, 4][..]));
    }

    #[test]
    fn align_does_not_skip_a_word_that_the_reader_holds_ahead() {
        let data = [0xFF, 0xFF, 1, 2, 3, 4];
        let mut b = Bits::new(&data);
        b.read(3);
        // The peek loads the second word, and reads no bit of it.
        b.peek16();
        b.read(10);
        b.align();
        assert_eq!(b.take_bytes(2), Some(&[1u8, 2][..]));
    }

    #[test]
    fn a_canonical_code_decodes_its_symbols() {
        // Lengths 2, 1, 3, 3 give the codes 10, 0, 110 and 111.
        let h = Huffman::new(&[2, 1, 3, 3]).unwrap();
        let data = pack("0 10 110 111 0 111");
        let mut b = Bits::new(&data);
        let got: Vec<u32> = (0..6).map(|_| h.decode(&mut b).unwrap()).collect();
        assert_eq!(got, [1, 0, 2, 3, 1, 3]);
    }

    #[test]
    fn a_long_code_decodes_past_the_fast_table() {
        // Symbol i has length i + 1, and the last two share 16.
        let mut lengths: Vec<u8> = (1..=16).collect();
        lengths.push(16);
        let h = Huffman::new(&lengths).unwrap();
        let ones = |n: usize| "1".repeat(n);
        let data = pack(&format!("{}0 {}1 {}0", ones(12), ones(15), ones(15)));
        let mut b = Bits::new(&data);
        assert_eq!(h.decode(&mut b).unwrap(), 12);
        assert_eq!(h.decode(&mut b).unwrap(), 16);
        assert_eq!(h.decode(&mut b).unwrap(), 15);
    }

    #[test]
    fn too_many_codes_are_refused_and_a_missing_code_fails() {
        assert!(Huffman::new(&[1, 1, 1]).unwrap_err().contains("more codes"));
        assert!(Huffman::new(&[17]).unwrap_err().contains("longer than"));
        let h = Huffman::new(&[1, 0]).unwrap();
        let data = pack("1");
        assert!(h.decode(&mut Bits::new(&data)).is_err());
    }
}
