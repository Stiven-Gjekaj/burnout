//! The LZMS decompressor: the compression of `install.esd` and of solid WIM
//! resources.
//!
//! # Origin and licence
//!
//! This is a port to Rust, by Stiven Gjekaj, of the LZMS decompressor of
//! wimlib (`lzms_decompress.c` and parts of `lzms_common.c`), which Eric
//! Biggers wrote. wimlib gives that code under the LGPL. On 2026-11-13 Eric
//! Biggers gave his permission to use this port under the MIT licence, which
//! is the licence of Burnout. The test encoder in `test_encoder.rs` ports
//! parts of `lzms_compress.c` under the same permission, and it is built for
//! the tests only.
//!
//! # The format
//!
//! LZMS is the compression of the Compression API of Windows 8 and later
//! (`COMPRESS_ALGORITHM_LZMS`). This module decompresses one raw block, as
//! `Decompress()` does with `COMPRESS_ALGORITHM_LZMS | COMPRESS_RAW`. The
//! block does not hold its uncompressed size, so the caller gives it. A WIM
//! file records it beside each chunk.
//!
//! The block is read as little-endian units of 16 bits from **both ends**:
//!
//! * A stream forwards from the start feeds an adaptive binary range decoder.
//!   It decides the type of each item: a literal, an LZ match or a delta
//!   match, with an explicit or a repeated offset.
//! * A stream backwards from the end gives adaptive Huffman symbols
//!   (literals, length slots, offset slots, delta powers) and verbatim extra
//!   bits, with the most significant bit first in each unit.
//!
//! After all items are decoded, an x86 filter is undone over the output.

#![forbid(unsafe_code)]

mod huffman;
mod tables;
mod x86;

#[cfg(test)]
mod test_encoder;

use huffman::{
    make_codeword_lengths, make_decode_table, DECODE_TABLE_LENGTH_MASK, DECODE_TABLE_SYMBOL_SHIFT,
};
use tables::{EXTRA_LENGTH_BITS, EXTRA_OFFSET_BITS, LENGTH_SLOT_BASE, OFFSET_SLOT_BASE};

// ---------------------------------------------------------------------------
// Constants (lzms_constants.h)
// ---------------------------------------------------------------------------

/// Largest uncompressed block size accepted (1 GiB), as in wimlib.
pub const MAX_BUFFER_SIZE: usize = 1 << 30;

const NUM_LZ_REPS: usize = 3;
const NUM_DELTA_REPS: usize = 3;
const NUM_LZ_REP_DECISIONS: usize = NUM_LZ_REPS - 1;
const NUM_DELTA_REP_DECISIONS: usize = NUM_DELTA_REPS - 1;

const NUM_MAIN_PROBS: u32 = 16;
const NUM_MATCH_PROBS: u32 = 32;
const NUM_LZ_PROBS: u32 = 64;
const NUM_LZ_REP_PROBS: u32 = 64;
const NUM_DELTA_PROBS: u32 = 64;
const NUM_DELTA_REP_PROBS: u32 = 64;

const PROBABILITY_BITS: u32 = 6;
const PROBABILITY_DENOMINATOR: u32 = 1 << PROBABILITY_BITS;
const INITIAL_PROBABILITY: u32 = 48;
const INITIAL_RECENT_BITS: u64 = 0x0000_0000_5555_5555;

const NUM_LITERAL_SYMS: usize = 256;
const NUM_LENGTH_SYMS: usize = 54;
const NUM_DELTA_POWER_SYMS: usize = 8;
const MAX_NUM_OFFSET_SYMS: usize = 799;
const MAX_NUM_SYMS: usize = 799;

const LITERAL_CODE_REBUILD_FREQ: u32 = 1024;
const LZ_OFFSET_CODE_REBUILD_FREQ: u32 = 1024;
const LENGTH_CODE_REBUILD_FREQ: u32 = 512;
const DELTA_OFFSET_CODE_REBUILD_FREQ: u32 = 1024;
const DELTA_POWER_CODE_REBUILD_FREQ: u32 = 512;

const MAX_CODEWORD_LENGTH: u32 = 15;

// Root-table sizes only affect speed. Decode-table sizes are the values of
// wimlib's DECODE_TABLE_ENOUGH() (computed with zlib's `enough` utility) for
// (num_syms, table_bits, max_codeword_len=15).
const LITERAL_TABLEBITS: u32 = 10;
const LENGTH_TABLEBITS: u32 = 9;
const LZ_OFFSET_TABLEBITS: u32 = 11;
const DELTA_OFFSET_TABLEBITS: u32 = 11;
const DELTA_POWER_TABLEBITS: u32 = 7;
const LITERAL_TABLE_SIZE: usize = 1302; // (256, 10)
const LENGTH_TABLE_SIZE: usize = 618; // (54, 9)
const OFFSET_TABLE_SIZE: usize = 2854; // (799, 11)
const DELTA_POWER_TABLE_SIZE: usize = 128; // (8, 7)

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Decompression failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The compressed buffer has odd length or is shorter than 4 bytes.
    InvalidInputLength,
    /// The requested output size exceeds [`MAX_BUFFER_SIZE`].
    OutputTooLarge,
    /// The compressed data is invalid (a match points before the start of the
    /// output, runs past its end, or a delta offset overflows).
    Corrupt,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::InvalidInputLength => "LZMS input must be an even number of bytes, at least 4",
            Error::OutputTooLarge => "LZMS output size exceeds 1 GiB",
            Error::Corrupt => "LZMS data is corrupt",
        })
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------------------
// Shared helpers (lzms_common.c)
// ---------------------------------------------------------------------------

/// Number of offset slots (= size of the LZ and delta offset alphabets) for a
/// block with the given uncompressed size.
pub(crate) fn num_offset_slots(uncompressed_size: usize) -> usize {
    if uncompressed_size < 2 {
        return 0;
    }
    1 + offset_slot((uncompressed_size - 1) as u32)
}

/// Slot whose range `[base[slot], base[slot + 1])` contains `value`.
pub(crate) fn offset_slot(value: u32) -> usize {
    OFFSET_SLOT_BASE[..MAX_NUM_OFFSET_SYMS].partition_point(|&b| b <= value) - 1
}

#[cfg(test)]
pub(crate) fn length_slot(value: u32) -> usize {
    LENGTH_SLOT_BASE[..NUM_LENGTH_SYMS].partition_point(|&b| b <= value) - 1
}

/// Adaptive probability for one range-coder context state: the probability
/// of a 0 bit is (number of zeros in the last 64 bits coded here) / 64.
#[derive(Clone, Copy)]
pub(crate) struct ProbabilityEntry {
    num_recent_zero_bits: u32,
    /// Most recent 64 bits; low-order bit is newest.
    recent_bits: u64,
}

impl ProbabilityEntry {
    const INIT: Self = Self {
        num_recent_zero_bits: INITIAL_PROBABILITY,
        recent_bits: INITIAL_RECENT_BITS,
    };

    /// Probability (out of 64) that the next bit is 0, clamped to [1, 63].
    #[inline]
    pub(crate) fn probability(&self) -> u32 {
        let mut prob = self.num_recent_zero_bits;
        // if prob == 0 { prob = 1 }
        prob += prob.wrapping_sub(1) >> 31;
        // if prob == 64 { prob = 63 }
        prob -= prob >> PROBABILITY_BITS;
        prob
    }

    #[inline]
    pub(crate) fn update(&mut self, bit: u32) {
        // The bit falling out of the 64-bit window was a zero iff its top bit
        // is 0; adjust the zero count for the bit leaving and the bit arriving.
        let delta = (self.recent_bits >> (PROBABILITY_DENOMINATOR - 1)) as i32 - bit as i32;
        self.num_recent_zero_bits = self.num_recent_zero_bits.wrapping_add(delta as u32);
        self.recent_bits = (self.recent_bits << 1) | bit as u64;
    }
}

#[derive(Clone)]
pub(crate) struct Probabilities {
    pub main: [ProbabilityEntry; NUM_MAIN_PROBS as usize],
    pub match_: [ProbabilityEntry; NUM_MATCH_PROBS as usize],
    pub lz: [ProbabilityEntry; NUM_LZ_PROBS as usize],
    pub delta: [ProbabilityEntry; NUM_DELTA_PROBS as usize],
    pub lz_rep: [[ProbabilityEntry; NUM_LZ_REP_PROBS as usize]; NUM_LZ_REP_DECISIONS],
    pub delta_rep: [[ProbabilityEntry; NUM_DELTA_REP_PROBS as usize]; NUM_DELTA_REP_DECISIONS],
}

impl Probabilities {
    pub(crate) fn new() -> Self {
        let e = ProbabilityEntry::INIT;
        Self {
            main: [e; 16],
            match_: [e; 32],
            lz: [e; 64],
            delta: [e; 64],
            lz_rep: [[e; 64]; NUM_LZ_REP_DECISIONS],
            delta_rep: [[e; 64]; NUM_DELTA_REP_DECISIONS],
        }
    }
}

// ---------------------------------------------------------------------------
// Forwards stream: range decoder
// ---------------------------------------------------------------------------

struct RangeDecoder<'a> {
    /// Relevant low part of the current range; kept above 0xffff.
    range: u32,
    /// Current code value within the range.
    code: u32,
    input: &'a [u8],
    /// Byte position of the next forwards 16-bit unit.
    next: usize,
}

impl<'a> RangeDecoder<'a> {
    /// Requires `input.len() >= 4`.
    fn new(input: &'a [u8]) -> Self {
        let code = (u32::from(u16::from_le_bytes([input[0], input[1]])) << 16)
            | u32::from(u16::from_le_bytes([input[2], input[3]]));
        Self {
            range: 0xffff_ffff,
            code,
            input,
            next: 4,
        }
    }

    /// Decode one bit using `probs[*state]`, then update both the probability
    /// entry and the context state (the last N bits decoded in this context,
    /// newest in the low-order bit).
    #[inline]
    fn decode_bit(
        &mut self,
        state: &mut u32,
        num_states: u32,
        probs: &mut [ProbabilityEntry],
    ) -> u32 {
        let entry = &mut probs[*state as usize];
        *state = (*state << 1) & (num_states - 1);

        let prob = entry.probability();

        // Normalize. Past the end of the input, zero bits are shifted in.
        if self.range & 0xffff_0000 == 0 {
            self.range <<= 16;
            self.code <<= 16;
            if self.next != self.input.len() {
                self.code |= u32::from(u16::from_le_bytes([
                    self.input[self.next],
                    self.input[self.next + 1],
                ]));
                self.next += 2;
            }
        }

        let bound = (self.range >> PROBABILITY_BITS) * prob;
        if self.code < bound {
            self.range = bound;
            entry.update(0);
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            entry.update(1);
            *state |= 1;
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Backwards stream: bit reader
// ---------------------------------------------------------------------------

struct InputBitstream<'a> {
    /// Buffered bits, left-aligned (next bit is bit 63).
    bitbuf: u64,
    bitsleft: u32,
    input: &'a [u8],
    /// Byte position just past the next unread 16-bit unit (reading backwards).
    next: usize,
}

impl<'a> InputBitstream<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            bitbuf: 0,
            bitsleft: 0,
            input,
            next: input.len(),
        }
    }

    /// Ensure at least `num_bits` (<= 32) bits are buffered. Once the start of
    /// the input is reached, zero bits are supplied (as in wimlib).
    #[inline]
    fn ensure_bits(&mut self, num_bits: u32) {
        if self.bitsleft >= num_bits {
            return;
        }
        while self.bitsleft <= 48 {
            let unit = if self.next >= 2 {
                self.next -= 2;
                u16::from_le_bytes([self.input[self.next], self.input[self.next + 1]])
            } else {
                0
            };
            self.bitbuf |= u64::from(unit) << (48 - self.bitsleft);
            self.bitsleft += 16;
        }
    }

    #[inline]
    fn peek_bits(&self, num_bits: u32) -> u32 {
        // Two shifts so that num_bits == 0 yields 0 without UB/overflow.
        ((self.bitbuf >> 1) >> (63 - num_bits)) as u32
    }

    #[inline]
    fn remove_bits(&mut self, num_bits: u32) {
        self.bitbuf <<= num_bits;
        self.bitsleft -= num_bits;
    }

    #[inline]
    fn read_bits(&mut self, num_bits: u32) -> u32 {
        self.ensure_bits(num_bits);
        let bits = self.peek_bits(num_bits);
        self.remove_bits(num_bits);
        bits
    }
}

// ---------------------------------------------------------------------------
// Adaptive Huffman codes
// ---------------------------------------------------------------------------

struct AdaptiveHuffman {
    num_syms: usize,
    rebuild_freq: u32,
    num_syms_until_rebuild: u32,
    table_bits: u32,
    freqs: Vec<u32>,
    lens: Vec<u8>,
    work: Vec<u32>,
    table: Vec<u16>,
}

impl AdaptiveHuffman {
    fn new(max_syms: usize, rebuild_freq: u32, table_bits: u32, table_size: usize) -> Self {
        Self {
            num_syms: 0,
            rebuild_freq,
            num_syms_until_rebuild: 0,
            table_bits,
            freqs: vec![0; max_syms],
            lens: vec![0; max_syms],
            work: vec![0; max_syms],
            table: vec![0; table_size],
        }
    }

    /// Reset for a new block: every symbol gets frequency 1.
    fn init(&mut self, num_syms: usize) {
        self.num_syms = num_syms;
        self.freqs.fill(0);
        self.freqs[..num_syms].fill(1);
        self.build();
    }

    fn build(&mut self) {
        let n = self.num_syms;
        if n >= 2 {
            make_codeword_lengths(
                &self.freqs[..n],
                MAX_CODEWORD_LENGTH,
                &mut self.lens[..n],
                &mut self.work,
            );
            let ok = make_decode_table(
                &mut self.table,
                self.table_bits,
                &self.lens[..n],
                MAX_CODEWORD_LENGTH,
            );
            debug_assert!(ok);
        } else {
            // Only reachable for the offset codes when the output is <= 2
            // bytes. wimlib's code builder requires >= 2 symbols and its
            // behaviour here is undefined; we use an empty code (always
            // decodes symbol 0 using 0 bits).
            self.table[..1 << self.table_bits].fill(0);
        }
        self.num_syms_until_rebuild = self.rebuild_freq;
    }

    /// Rebuild from the current frequencies, *then* halve them so the code
    /// favours recent data.
    #[cold]
    fn rebuild(&mut self) {
        self.build();
        for f in &mut self.freqs[..self.num_syms] {
            *f = (*f >> 1) + 1;
        }
    }

    #[inline]
    fn decode(&mut self, is: &mut InputBitstream) -> usize {
        is.ensure_bits(MAX_CODEWORD_LENGTH);

        let mut entry = self.table[is.peek_bits(self.table_bits) as usize];
        let mut symbol = (entry >> DECODE_TABLE_SYMBOL_SHIFT) as usize;
        let mut length = (entry & DECODE_TABLE_LENGTH_MASK) as u32;

        if u32::from(entry) >= 1 << (self.table_bits + DECODE_TABLE_SYMBOL_SHIFT) {
            // Subtable pointer.
            is.remove_bits(self.table_bits);
            entry = self.table[symbol + is.peek_bits(length) as usize];
            symbol = (entry >> DECODE_TABLE_SYMBOL_SHIFT) as usize;
            length = (entry & DECODE_TABLE_LENGTH_MASK) as u32;
        }
        is.remove_bits(length);

        self.freqs[symbol] += 1;
        self.num_syms_until_rebuild -= 1;
        if self.num_syms_until_rebuild == 0 {
            self.rebuild();
        }
        symbol
    }
}

// ---------------------------------------------------------------------------
// Decompressor
// ---------------------------------------------------------------------------

/// Reusable LZMS decompressor. Holds ~300 KiB of tables; reuse it across
/// blocks to avoid reallocating.
pub struct Decompressor {
    probs: Probabilities,
    literal: AdaptiveHuffman,
    lz_offset: AdaptiveHuffman,
    length: AdaptiveHuffman,
    delta_offset: AdaptiveHuffman,
    delta_power: AdaptiveHuffman,
    last_target_usages: Box<[i32; 65536]>,
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Decompressor {
    pub fn new() -> Self {
        Self {
            probs: Probabilities::new(),
            literal: AdaptiveHuffman::new(
                NUM_LITERAL_SYMS,
                LITERAL_CODE_REBUILD_FREQ,
                LITERAL_TABLEBITS,
                LITERAL_TABLE_SIZE,
            ),
            lz_offset: AdaptiveHuffman::new(
                MAX_NUM_OFFSET_SYMS,
                LZ_OFFSET_CODE_REBUILD_FREQ,
                LZ_OFFSET_TABLEBITS,
                OFFSET_TABLE_SIZE,
            ),
            length: AdaptiveHuffman::new(
                NUM_LENGTH_SYMS,
                LENGTH_CODE_REBUILD_FREQ,
                LENGTH_TABLEBITS,
                LENGTH_TABLE_SIZE,
            ),
            delta_offset: AdaptiveHuffman::new(
                MAX_NUM_OFFSET_SYMS,
                DELTA_OFFSET_CODE_REBUILD_FREQ,
                DELTA_OFFSET_TABLEBITS,
                OFFSET_TABLE_SIZE,
            ),
            delta_power: AdaptiveHuffman::new(
                NUM_DELTA_POWER_SYMS,
                DELTA_POWER_CODE_REBUILD_FREQ,
                DELTA_POWER_TABLEBITS,
                DELTA_POWER_TABLE_SIZE,
            ),
            last_target_usages: vec![0i32; 65536]
                .into_boxed_slice()
                .try_into()
                .expect("65536 entries"),
        }
    }

    #[inline]
    fn decode_lz_offset(&mut self, is: &mut InputBitstream) -> u32 {
        let slot = self.lz_offset.decode(is);
        OFFSET_SLOT_BASE[slot] + is.read_bits(u32::from(EXTRA_OFFSET_BITS[slot]))
    }

    #[inline]
    fn decode_delta_offset(&mut self, is: &mut InputBitstream) -> u32 {
        let slot = self.delta_offset.decode(is);
        OFFSET_SLOT_BASE[slot] + is.read_bits(u32::from(EXTRA_OFFSET_BITS[slot]))
    }

    #[inline]
    fn decode_length(&mut self, is: &mut InputBitstream) -> u32 {
        let slot = self.length.decode(is);
        let extra = u32::from(EXTRA_LENGTH_BITS[slot]);
        let mut length = LENGTH_SLOT_BASE[slot];
        if extra != 0 {
            length += is.read_bits(extra);
        }
        length
    }

    /// Decompress one raw LZMS block. `output.len()` must be exactly the
    /// original uncompressed size.
    pub fn decompress(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), Error> {
        // LZMS data is a sequence of 16-bit units, and the range decoder needs
        // at least 32 bits to start.
        if input.len() & 1 != 0 || input.len() < 4 {
            return Err(Error::InvalidInputLength);
        }
        if output.len() > MAX_BUFFER_SIZE {
            return Err(Error::OutputTooLarge);
        }

        let out_len = output.len();
        let mut rd = RangeDecoder::new(input);
        let mut is = InputBitstream::new(input);

        self.probs = Probabilities::new();
        let n_off = num_offset_slots(out_len);
        self.literal.init(NUM_LITERAL_SYMS);
        self.lz_offset.init(n_off);
        self.length.init(NUM_LENGTH_SYMS);
        self.delta_offset.init(n_off);
        self.delta_power.init(NUM_DELTA_POWER_SYMS);

        // LRU queues of match sources. The 4th slot is overflow: entries
        // are removed on use, and insertion at the front is delayed by one
        // item (see below).
        let mut recent_lz_offsets: [u32; NUM_LZ_REPS + 1] = [1, 2, 3, 4];
        // Delta pairs packed as (power << 32) | raw_offset.
        let mut recent_delta_pairs: [u64; NUM_DELTA_REPS + 1] = [1, 2, 3, 4];

        // Previous item type: 0 = literal, 1 = LZ match, 2 = delta match.
        // Instead of physically delaying the front-insertion of the last-used
        // source, the queue is updated immediately and a rep lookup made
        // right after a match of the same kind reads slot `rep_idx + 1`.
        let mut prev_item_type: usize = 0;

        let mut main_state = 0u32;
        let mut match_state = 0u32;
        let mut lz_state = 0u32;
        let mut delta_state = 0u32;
        let mut lz_rep_states = [0u32; NUM_LZ_REP_DECISIONS];
        let mut delta_rep_states = [0u32; NUM_DELTA_REP_DECISIONS];

        let mut pos = 0usize;
        while pos != out_len {
            if rd.decode_bit(&mut main_state, NUM_MAIN_PROBS, &mut self.probs.main) == 0 {
                // ---- Literal ----
                output[pos] = self.literal.decode(&mut is) as u8;
                pos += 1;
                prev_item_type = 0;
            } else if rd.decode_bit(&mut match_state, NUM_MATCH_PROBS, &mut self.probs.match_) == 0
            {
                // ---- LZ match ----
                let offset;
                if rd.decode_bit(&mut lz_state, NUM_LZ_PROBS, &mut self.probs.lz) == 0 {
                    // Explicit offset.
                    offset = self.decode_lz_offset(&mut is);
                    recent_lz_offsets[3] = recent_lz_offsets[2];
                    recent_lz_offsets[2] = recent_lz_offsets[1];
                    recent_lz_offsets[1] = recent_lz_offsets[0];
                } else {
                    // Repeat offset. `d` accounts for a delayed insertion.
                    let d = prev_item_type & 1;
                    if rd.decode_bit(
                        &mut lz_rep_states[0],
                        NUM_LZ_REP_PROBS,
                        &mut self.probs.lz_rep[0],
                    ) == 0
                    {
                        offset = recent_lz_offsets[d];
                        recent_lz_offsets[d] = recent_lz_offsets[0];
                    } else if rd.decode_bit(
                        &mut lz_rep_states[1],
                        NUM_LZ_REP_PROBS,
                        &mut self.probs.lz_rep[1],
                    ) == 0
                    {
                        offset = recent_lz_offsets[1 + d];
                        recent_lz_offsets[1 + d] = recent_lz_offsets[1];
                        recent_lz_offsets[1] = recent_lz_offsets[0];
                    } else {
                        offset = recent_lz_offsets[2 + d];
                        recent_lz_offsets[2 + d] = recent_lz_offsets[2];
                        recent_lz_offsets[2] = recent_lz_offsets[1];
                        recent_lz_offsets[1] = recent_lz_offsets[0];
                    }
                }
                recent_lz_offsets[0] = offset;
                prev_item_type = 1;

                let length = self.decode_length(&mut is) as usize;
                let offset = offset as usize;

                if offset > pos || length > out_len - pos {
                    return Err(Error::Corrupt);
                }
                lz_copy(output, pos, offset, length);
                pos += length;
            } else {
                // ---- Delta match ----
                let power: u32;
                let raw_offset: u32;
                let pair: u64;
                if rd.decode_bit(&mut delta_state, NUM_DELTA_PROBS, &mut self.probs.delta) == 0 {
                    // Explicit offset. (Power is decoded before the offset.)
                    power = self.delta_power.decode(&mut is) as u32;
                    raw_offset = self.decode_delta_offset(&mut is);
                    pair = (u64::from(power) << 32) | u64::from(raw_offset);
                    recent_delta_pairs[3] = recent_delta_pairs[2];
                    recent_delta_pairs[2] = recent_delta_pairs[1];
                    recent_delta_pairs[1] = recent_delta_pairs[0];
                } else {
                    let d = prev_item_type >> 1;
                    if rd.decode_bit(
                        &mut delta_rep_states[0],
                        NUM_DELTA_REP_PROBS,
                        &mut self.probs.delta_rep[0],
                    ) == 0
                    {
                        pair = recent_delta_pairs[d];
                        recent_delta_pairs[d] = recent_delta_pairs[0];
                    } else if rd.decode_bit(
                        &mut delta_rep_states[1],
                        NUM_DELTA_REP_PROBS,
                        &mut self.probs.delta_rep[1],
                    ) == 0
                    {
                        pair = recent_delta_pairs[1 + d];
                        recent_delta_pairs[1 + d] = recent_delta_pairs[1];
                        recent_delta_pairs[1] = recent_delta_pairs[0];
                    } else {
                        pair = recent_delta_pairs[2 + d];
                        recent_delta_pairs[2 + d] = recent_delta_pairs[2];
                        recent_delta_pairs[2] = recent_delta_pairs[1];
                        recent_delta_pairs[1] = recent_delta_pairs[0];
                    }
                    power = (pair >> 32) as u32;
                    raw_offset = pair as u32;
                }
                recent_delta_pairs[0] = pair;
                prev_item_type = 2;

                let length = self.decode_length(&mut is) as usize;

                let span = 1u32 << power;
                let offset = raw_offset << power;
                // raw_offset << power overflowed?
                if offset >> power != raw_offset {
                    return Err(Error::Corrupt);
                }
                // offset + span overflowed?
                let Some(reach) = offset.checked_add(span) else {
                    return Err(Error::Corrupt);
                };
                // Buffer underrun / overrun?
                if reach as usize > pos || length > out_len - pos {
                    return Err(Error::Corrupt);
                }

                // A[i] = B[i] + C[i] - D[i], byte-at-a-time because the
                // sequences may overlap the bytes being written.
                let (offset, span) = (offset as usize, span as usize);
                for k in pos..pos + length {
                    output[k] = output[k - offset]
                        .wrapping_add(output[k - span])
                        .wrapping_sub(output[k - offset - span]);
                }
                pos += length;
            }
        }

        x86::x86_filter(output, &mut self.last_target_usages, true);
        Ok(())
    }
}

/// Copy `length` bytes from `offset` bytes back, with LZ77 overlap semantics.
#[inline]
fn lz_copy(out: &mut [u8], pos: usize, offset: usize, length: usize) {
    if offset >= length {
        out.copy_within(pos - offset..pos - offset + length, pos);
    } else {
        for k in pos..pos + length {
            out[k] = out[k - offset];
        }
    }
}

/// Decompress one raw LZMS block into `output`, whose length must be exactly
/// the uncompressed size.
pub fn decompress(input: &[u8], output: &mut [u8]) -> Result<(), Error> {
    Decompressor::new().decompress(input, output)
}

/// Decompress one raw LZMS block of known uncompressed size into a new `Vec`.
pub fn decompress_to_vec(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, Error> {
    if uncompressed_size > MAX_BUFFER_SIZE {
        return Err(Error::OutputTooLarge);
    }
    let mut out = vec![0u8; uncompressed_size];
    decompress(input, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_tables_are_consistent() {
        for i in 0..MAX_NUM_OFFSET_SYMS {
            assert_eq!(
                OFFSET_SLOT_BASE[i + 1] - OFFSET_SLOT_BASE[i],
                1 << EXTRA_OFFSET_BITS[i]
            );
        }
        for i in 0..NUM_LENGTH_SYMS {
            assert_eq!(
                LENGTH_SLOT_BASE[i + 1] - LENGTH_SLOT_BASE[i],
                1 << EXTRA_LENGTH_BITS[i]
            );
        }
        assert_eq!(OFFSET_SLOT_BASE[MAX_NUM_OFFSET_SYMS], 1_180_427_428 + 1);
        assert_eq!(LENGTH_SLOT_BASE[NUM_LENGTH_SYMS], 1_073_809_578 + 1);
    }

    #[test]
    fn probability_entry_matches_spec() {
        let mut e = ProbabilityEntry::INIT;
        assert_eq!(e.probability(), 48);
        for _ in 0..64 {
            e.update(1);
        }
        assert_eq!(e.num_recent_zero_bits, 0);
        assert_eq!(e.probability(), 1); // clamped
        for _ in 0..64 {
            e.update(0);
        }
        assert_eq!(e.num_recent_zero_bits, 64);
        assert_eq!(e.probability(), 63); // clamped
    }

    #[test]
    fn rejects_bad_lengths() {
        let mut out = [0u8; 4];
        assert_eq!(
            decompress(&[0; 3], &mut out),
            Err(Error::InvalidInputLength)
        );
        assert_eq!(
            decompress(&[0; 2], &mut out),
            Err(Error::InvalidInputLength)
        );
    }

    #[test]
    fn num_offset_slots_small() {
        assert_eq!(num_offset_slots(0), 0);
        assert_eq!(num_offset_slots(1), 0);
        assert_eq!(num_offset_slots(2), 1);
        assert_eq!(num_offset_slots(9), 8);
        assert_eq!(num_offset_slots(10), 9);
    }
}
