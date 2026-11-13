//! Test-only LZMS encoder.
//!
//! The bit-level output routines are ported from wimlib's `lzms_compress.c`
//! (`lzms_range_encoder_*`, `lzms_write_bits`, `lzms_output_bitstream_flush`,
//! `lzms_encode_item`, `lzms_finalize`), so round-trip tests check the decoder
//! against the reference encoder's layout rather than against itself. Match
//! finding is a simple greedy search, built to exercise every item type
//! (literals, explicit/repeat LZ, explicit/repeat delta, long lengths and
//! offsets) rather than to compress well.
//!
//! Repeat-offset bookkeeping uses wimlib's *encoder* model (`prev`/`upcoming`
//! delayed insertion), which is independent of the decoder's
//! `prev_item_type` shortcut; every repeat lookup asserts both models agree.

// The loops over the repeat queues follow wimlib line by line, so an index
// stays easier to compare with the C code than an iterator.
#![allow(clippy::needless_range_loop)]

use super::huffman::make_codeword_lengths;
use super::tables::*;
use super::*;

// ----------------------------------------------------------------------------
// Range encoder (forwards)
// ----------------------------------------------------------------------------

struct RangeEncoder {
    lower_bound: u64,
    range_size: u32,
    cache: u16,
    cache_size: u32,
    /// wimlib starts `next` one unit *before* the buffer so that the first
    /// (dummy) cache unit is discarded.
    skip_first: bool,
    out: Vec<u8>,
}

impl RangeEncoder {
    fn new() -> Self {
        Self {
            lower_bound: 0,
            range_size: 0xffff_ffff,
            cache: 0,
            cache_size: 1,
            skip_first: true,
            out: Vec::new(),
        }
    }

    fn shift_low(&mut self) {
        if (self.lower_bound as u32) < 0xffff_0000 || (self.lower_bound >> 32) as u32 != 0 {
            loop {
                if self.skip_first {
                    self.skip_first = false;
                } else {
                    let v = self.cache.wrapping_add((self.lower_bound >> 32) as u16);
                    self.out.extend_from_slice(&v.to_le_bytes());
                }
                self.cache = 0xffff;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.lower_bound >> 16) & 0xffff) as u16;
        }
        self.cache_size += 1;
        self.lower_bound = (self.lower_bound & 0xffff) << 16;
    }

    fn flush(&mut self) {
        for _ in 0..4 {
            self.shift_low();
        }
    }

    fn encode_bit(&mut self, bit: u32, prob: u32) {
        if self.range_size <= 0xffff {
            self.range_size <<= 16;
            self.shift_low();
        }
        let bound = (self.range_size >> PROBABILITY_BITS) * prob;
        if bit == 0 {
            self.range_size = bound;
        } else {
            self.lower_bound += u64::from(bound);
            self.range_size -= bound;
        }
    }

    fn encode_ctx_bit(
        &mut self,
        bit: u32,
        state: &mut u32,
        num_states: u32,
        probs: &mut [ProbabilityEntry],
    ) {
        let entry = &mut probs[*state as usize];
        *state = ((*state << 1) | bit) & (num_states - 1);
        let prob = entry.probability();
        entry.update(bit);
        self.encode_bit(bit, prob);
    }
}

// ----------------------------------------------------------------------------
// Output bitstream (backwards)
// ----------------------------------------------------------------------------

struct OutputBitstream {
    bitbuf: u64,
    bitcount: u32,
    /// Units in the order written; they are laid out back-to-front.
    units: Vec<u16>,
}

impl OutputBitstream {
    fn new() -> Self {
        Self {
            bitbuf: 0,
            bitcount: 0,
            units: Vec::new(),
        }
    }

    fn write_bits(&mut self, bits: u32, num_bits: u32) {
        self.bitcount += num_bits;
        self.bitbuf = (self.bitbuf << num_bits) | u64::from(bits);
        while self.bitcount >= 16 {
            self.bitcount -= 16;
            self.units.push((self.bitbuf >> self.bitcount) as u16);
        }
    }

    fn flush(&mut self) {
        if self.bitcount != 0 {
            self.units
                .push((self.bitbuf << (16 - self.bitcount)) as u16);
        }
    }
}

// ----------------------------------------------------------------------------
// Adaptive Huffman encoder
// ----------------------------------------------------------------------------

struct HuffEnc {
    num_syms: usize,
    rebuild_freq: u32,
    until: u32,
    freqs: Vec<u32>,
    lens: Vec<u8>,
    codewords: Vec<u32>,
    work: Vec<u32>,
}

impl HuffEnc {
    fn new(num_syms: usize, rebuild_freq: u32) -> Self {
        let n = num_syms.max(1);
        let mut h = Self {
            num_syms,
            rebuild_freq,
            until: 0,
            freqs: vec![1; n],
            lens: vec![0; n],
            codewords: vec![0; n],
            work: vec![0; n],
        };
        h.build();
        h
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
            // Canonical codewords (second half of wimlib's gen_codewords()).
            let mut counts = [0u32; 16];
            for &l in &self.lens[..n] {
                counts[l as usize] += 1;
            }
            let mut next = [0u32; 16];
            for len in 2..16 {
                next[len] = (next[len - 1] + counts[len - 1]) << 1;
            }
            for s in 0..n {
                let l = self.lens[s] as usize;
                self.codewords[s] = next[l];
                next[l] += 1;
            }
        }
        self.until = self.rebuild_freq;
    }

    fn encode(&mut self, sym: usize, os: &mut OutputBitstream) {
        assert!(self.num_syms >= 2 && sym < self.num_syms);
        os.write_bits(self.codewords[sym], u32::from(self.lens[sym]));
        self.freqs[sym] += 1;
        self.until -= 1;
        if self.until == 0 {
            self.build();
            for f in &mut self.freqs[..self.num_syms] {
                *f = (*f >> 1) + 1;
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Items and source-queue models
// ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Item {
    Literal,
    LzExplicit { offset: u32, len: u32 },
    LzRep { idx: usize, len: u32 },
    DeltaExplicit { power: u32, raw: u32, len: u32 },
    DeltaRep { idx: usize, len: u32 },
}

/// wimlib encoder model (lzms_adaptive_state + lzms_update_lru_queues).
#[derive(Clone)]
struct WimlibQueues {
    lz: [u32; 4],
    prev_lz: u32,
    delta: [u64; 4],
    prev_delta: u64,
}

impl WimlibQueues {
    fn new() -> Self {
        Self {
            lz: [1, 2, 3, 4],
            prev_lz: 0,
            delta: [1, 2, 3, 4],
            prev_delta: 0,
        }
    }
    fn advance(&mut self, upcoming_lz: u32, upcoming_delta: u64) {
        if self.prev_lz != 0 {
            for i in (0..3).rev() {
                self.lz[i + 1] = self.lz[i];
            }
            self.lz[0] = self.prev_lz;
        }
        self.prev_lz = upcoming_lz;
        if self.prev_delta != 0 {
            for i in (0..3).rev() {
                self.delta[i + 1] = self.delta[i];
            }
            self.delta[0] = self.prev_delta;
        }
        self.prev_delta = upcoming_delta;
    }
    fn take_lz(&mut self, idx: usize) -> u32 {
        let off = self.lz[idx];
        for i in idx..3 {
            self.lz[i] = self.lz[i + 1];
        }
        off
    }
    fn take_delta(&mut self, idx: usize) -> u64 {
        let p = self.delta[idx];
        for i in idx..3 {
            self.delta[i] = self.delta[i + 1];
        }
        p
    }
}

/// Decoder model, copied from the decompressor, used only as a cross-check.
#[derive(Clone)]
struct DecoderQueues {
    lz: [u32; 4],
    delta: [u64; 4],
    prev_type: usize,
}

impl DecoderQueues {
    fn new() -> Self {
        Self {
            lz: [1, 2, 3, 4],
            delta: [1, 2, 3, 4],
            prev_type: 0,
        }
    }
    fn peek_lz(&self, idx: usize) -> u32 {
        self.lz[idx + (self.prev_type & 1)]
    }
    fn peek_delta(&self, idx: usize) -> u64 {
        self.delta[idx + (self.prev_type >> 1)]
    }
    fn apply(&mut self, item: Item) {
        match item {
            Item::Literal => self.prev_type = 0,
            Item::LzExplicit { offset, .. } => {
                self.lz.copy_within(0..3, 1);
                self.lz[0] = offset;
                self.prev_type = 1;
            }
            Item::LzRep { idx, .. } => {
                let d = self.prev_type & 1;
                let off = self.lz[idx + d];
                self.lz[idx + d] = self.lz[idx];
                for i in (1..=idx).rev() {
                    self.lz[i] = self.lz[i - 1];
                }
                self.lz[0] = off;
                self.prev_type = 1;
            }
            Item::DeltaExplicit { power, raw, .. } => {
                self.delta.copy_within(0..3, 1);
                self.delta[0] = (u64::from(power) << 32) | u64::from(raw);
                self.prev_type = 2;
            }
            Item::DeltaRep { idx, .. } => {
                let d = self.prev_type >> 1;
                let p = self.delta[idx + d];
                self.delta[idx + d] = self.delta[idx];
                for i in (1..=idx).rev() {
                    self.delta[i] = self.delta[i - 1];
                }
                self.delta[0] = p;
                self.prev_type = 2;
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Encoder
// ----------------------------------------------------------------------------

pub struct Options {
    /// Allow delta matches.
    pub delta: bool,
    /// Max match length (useful for forcing many short items).
    pub max_len: u32,
    /// Allow length-1 repeat matches (legal, though wimlib never emits them).
    pub len1_reps: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            delta: true,
            max_len: LENGTH_SLOT_BASE[NUM_LENGTH_SYMS] - 1,
            len1_reps: false,
        }
    }
}

fn lz_len(d: &[u8], pos: usize, off: usize, max: usize) -> usize {
    if off == 0 || off > pos {
        return 0;
    }
    let mut n = 0;
    while n < max && d[pos + n] == d[pos + n - off] {
        n += 1;
    }
    n
}

fn delta_len(d: &[u8], pos: usize, power: u32, raw: u32, max: usize) -> usize {
    let span = 1usize << power;
    let Some(off) = (raw as usize).checked_shl(power) else {
        return 0;
    };
    if raw == 0 || off + span > pos {
        return 0;
    }
    let mut n = 0;
    while n < max {
        let k = pos + n;
        let pred = d[k - off]
            .wrapping_add(d[k - span])
            .wrapping_sub(d[k - off - span]);
        if d[k] != pred {
            break;
        }
        n += 1;
    }
    n
}

pub fn compress(input: &[u8], opts: &Options) -> Vec<u8> {
    compress_with_stats(input, opts).0
}

/// Returns the compressed data and counts of [literal, lz explicit, lz rep, delta explicit, delta rep].
pub fn compress_with_stats(input: &[u8], opts: &Options) -> (Vec<u8>, [usize; 5]) {
    let mut stats = [0usize; 5];
    let mut data = input.to_vec();
    let mut ltu = Box::new([0i32; 65536]);
    super::x86::x86_filter(&mut data, &mut ltu, false);
    let d = &data[..];
    let n = d.len();

    let mut rc = RangeEncoder::new();
    let mut os = OutputBitstream::new();
    let mut probs = Probabilities::new();
    let n_off = num_offset_slots(n);
    let mut lit = HuffEnc::new(NUM_LITERAL_SYMS, LITERAL_CODE_REBUILD_FREQ);
    let mut lzo = HuffEnc::new(n_off, LZ_OFFSET_CODE_REBUILD_FREQ);
    let mut lenc = HuffEnc::new(NUM_LENGTH_SYMS, LENGTH_CODE_REBUILD_FREQ);
    let mut dlo = HuffEnc::new(n_off, DELTA_OFFSET_CODE_REBUILD_FREQ);
    let mut dpw = HuffEnc::new(NUM_DELTA_POWER_SYMS, DELTA_POWER_CODE_REBUILD_FREQ);

    let (mut main_s, mut match_s, mut lz_s, mut delta_s) = (0u32, 0u32, 0u32, 0u32);
    let mut lz_rep_s = [0u32; 2];
    let mut delta_rep_s = [0u32; 2];

    let mut wq = WimlibQueues::new();
    let mut dq = DecoderQueues::new();

    // Hash chains over 3-byte prefixes for explicit LZ candidates.
    const HBITS: u32 = 15;
    let mut head = vec![usize::MAX; 1 << HBITS];
    let mut prev = vec![usize::MAX; n];
    let hash = |p: usize| -> usize {
        let v = u32::from(d[p]) | u32::from(d[p + 1]) << 8 | u32::from(d[p + 2]) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HBITS)) as usize
    };
    let mut inserted = 0usize;

    let can_match = n_off >= 2;
    let mut pos = 0usize;
    while pos < n {
        while inserted + 3 <= n && inserted <= pos {
            let h = hash(inserted);
            prev[inserted] = head[h];
            head[h] = inserted;
            inserted += 1;
        }
        let max = (n - pos).min(opts.max_len as usize);
        let mut best = Item::Literal;
        let mut best_len = 1usize; // a match must beat this
        if can_match {
            for idx in 0..3 {
                let off = wq.lz[idx];
                assert_eq!(off, dq.peek_lz(idx), "LZ queue models disagree");
                let l = lz_len(d, pos, off as usize, max);
                if l > best_len || (opts.len1_reps && l == 1 && matches!(best, Item::Literal)) {
                    best_len = l;
                    best = Item::LzRep { idx, len: l as u32 };
                }
            }
            if opts.delta {
                for idx in 0..3 {
                    let pair = wq.delta[idx];
                    assert_eq!(pair, dq.peek_delta(idx), "delta queue models disagree");
                    let l = delta_len(d, pos, (pair >> 32) as u32, pair as u32, max);
                    if l > best_len + 1 {
                        best_len = l;
                        best = Item::DeltaRep { idx, len: l as u32 };
                    }
                }
            }
            if pos + 3 <= n {
                let mut cand = head[hash(pos)];
                let mut chain = 0;
                while cand != usize::MAX && chain < 32 {
                    if cand < pos {
                        let off = pos - cand;
                        let l = lz_len(d, pos, off, max);
                        // Prefer reps: an explicit match must be longer.
                        if l > best_len + 1 {
                            best_len = l;
                            best = Item::LzExplicit {
                                offset: off as u32,
                                len: l as u32,
                            };
                        }
                    }
                    cand = prev[cand];
                    chain += 1;
                }
            }
            if opts.delta {
                for power in 0..8u32 {
                    for raw in 1..=4u32 {
                        let l = delta_len(d, pos, power, raw, max);
                        if l > best_len + 2 {
                            best_len = l;
                            best = Item::DeltaExplicit {
                                power,
                                raw,
                                len: l as u32,
                            };
                        }
                    }
                }
            }
        }

        // ---- Encode the chosen item (lzms_encode_item) ----
        let is_match = !matches!(best, Item::Literal);
        rc.encode_ctx_bit(
            is_match as u32,
            &mut main_s,
            NUM_MAIN_PROBS,
            &mut probs.main,
        );
        let len = match best {
            Item::Literal => {
                lit.encode(d[pos] as usize, &mut os);
                wq.advance(0, 0);
                1
            }
            Item::LzExplicit { offset, len } => {
                rc.encode_ctx_bit(0, &mut match_s, NUM_MATCH_PROBS, &mut probs.match_);
                rc.encode_ctx_bit(0, &mut lz_s, NUM_LZ_PROBS, &mut probs.lz);
                let slot = offset_slot(offset);
                lzo.encode(slot, &mut os);
                os.write_bits(
                    offset - OFFSET_SLOT_BASE[slot],
                    u32::from(EXTRA_OFFSET_BITS[slot]),
                );
                wq.advance(offset, 0);
                len
            }
            Item::LzRep { idx, len } => {
                rc.encode_ctx_bit(0, &mut match_s, NUM_MATCH_PROBS, &mut probs.match_);
                rc.encode_ctx_bit(1, &mut lz_s, NUM_LZ_PROBS, &mut probs.lz);
                for i in 0..idx {
                    rc.encode_ctx_bit(1, &mut lz_rep_s[i], NUM_LZ_REP_PROBS, &mut probs.lz_rep[i]);
                }
                if idx < NUM_LZ_REP_DECISIONS {
                    rc.encode_ctx_bit(
                        0,
                        &mut lz_rep_s[idx],
                        NUM_LZ_REP_PROBS,
                        &mut probs.lz_rep[idx],
                    );
                }
                let off = wq.take_lz(idx);
                wq.advance(off, 0);
                len
            }
            Item::DeltaExplicit { power, raw, len } => {
                rc.encode_ctx_bit(1, &mut match_s, NUM_MATCH_PROBS, &mut probs.match_);
                rc.encode_ctx_bit(0, &mut delta_s, NUM_DELTA_PROBS, &mut probs.delta);
                dpw.encode(power as usize, &mut os);
                let slot = offset_slot(raw);
                dlo.encode(slot, &mut os);
                os.write_bits(
                    raw - OFFSET_SLOT_BASE[slot],
                    u32::from(EXTRA_OFFSET_BITS[slot]),
                );
                wq.advance(0, (u64::from(power) << 32) | u64::from(raw));
                len
            }
            Item::DeltaRep { idx, len } => {
                rc.encode_ctx_bit(1, &mut match_s, NUM_MATCH_PROBS, &mut probs.match_);
                rc.encode_ctx_bit(1, &mut delta_s, NUM_DELTA_PROBS, &mut probs.delta);
                for i in 0..idx {
                    rc.encode_ctx_bit(
                        1,
                        &mut delta_rep_s[i],
                        NUM_DELTA_REP_PROBS,
                        &mut probs.delta_rep[i],
                    );
                }
                if idx < NUM_DELTA_REP_DECISIONS {
                    rc.encode_ctx_bit(
                        0,
                        &mut delta_rep_s[idx],
                        NUM_DELTA_REP_PROBS,
                        &mut probs.delta_rep[idx],
                    );
                }
                let pair = wq.take_delta(idx);
                wq.advance(0, pair);
                len
            }
        };
        if is_match {
            let slot = length_slot(len);
            lenc.encode(slot, &mut os);
            let extra = u32::from(EXTRA_LENGTH_BITS[slot]);
            if extra != 0 {
                os.write_bits(len - LENGTH_SLOT_BASE[slot], extra);
            }
        }
        stats[match best {
            Item::Literal => 0,
            Item::LzExplicit { .. } => 1,
            Item::LzRep { .. } => 2,
            Item::DeltaExplicit { .. } => 3,
            Item::DeltaRep { .. } => 4,
        }] += 1;
        dq.apply(best);
        pos += len as usize;
    }

    // lzms_finalize(): forwards data, then the backwards units laid out from
    // the end of the buffer towards the front.
    os.flush();
    rc.flush();
    let mut out = rc.out;
    for u in os.units.iter().rev() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    (out, stats)
}

// ----------------------------------------------------------------------------
// Round-trip and robustness tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64* — deterministic test data without extra dependencies.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn byte(&mut self) -> u8 {
            (self.next() >> 56) as u8
        }
    }

    fn roundtrip_with(data: &[u8], opts: &Options) -> usize {
        let c = compress(data, opts);
        assert!(
            c.len() % 2 == 0 && c.len() >= 4,
            "bad compressed length {}",
            c.len()
        );
        let d = decompress_to_vec(&c, data.len()).expect("decompress failed");
        assert!(d == data, "round-trip mismatch (len {})", data.len());
        c.len()
    }

    fn roundtrip(data: &[u8]) -> usize {
        roundtrip_with(data, &Options::default())
    }

    #[test]
    fn tiny_inputs() {
        for n in 0..40 {
            let data: Vec<u8> = (0..n).map(|i| b"abracadabra"[i % 11]).collect();
            roundtrip(&data);
        }
    }

    #[test]
    fn text_and_repeats() {
        let text = b"It was the best of times, it was the worst of times, it was the age of wisdom, \
                     it was the age of foolishness, it was the epoch of belief, it was the epoch of incredulity. ";
        let mut data = Vec::new();
        for _ in 0..200 {
            data.extend_from_slice(text);
        }
        let c = roundtrip(&data);
        assert!(c < data.len() / 10, "should compress well, got {c}");
    }

    #[test]
    fn random_data_exercises_literal_rebuilds() {
        let mut r = Rng(1);
        let data: Vec<u8> = (0..300_000).map(|_| r.byte()).collect();
        roundtrip(&data);
    }

    #[test]
    fn short_items_and_len1_reps() {
        // Lots of short matches -> many rep-queue transitions and rebuilds.
        let mut r = Rng(7);
        let mut data = Vec::new();
        while data.len() < 200_000 {
            let k = (r.next() % 6) as usize;
            if k == 0 || data.len() < 64 {
                data.push(r.byte() & 0x0f);
            } else {
                let off = 1 + (r.next() % 48) as usize;
                let off = off.min(data.len());
                for _ in 0..(1 + r.next() % 5) {
                    let b = data[data.len() - off];
                    data.push(b);
                }
            }
        }
        roundtrip_with(
            &data,
            &Options {
                max_len: 7,
                len1_reps: true,
                ..Default::default()
            },
        );
        roundtrip_with(
            &data,
            &Options {
                delta: false,
                ..Default::default()
            },
        );
    }

    #[test]
    fn delta_matches_on_ramps() {
        // 16-bit little-endian ramp interleaved with a 32-bit ramp and noise:
        // ideal for delta matches with several powers.
        let mut r = Rng(3);
        let mut data = Vec::new();
        for i in 0..30_000u32 {
            data.extend_from_slice(&((i * 3) as u16).to_le_bytes());
            if i % 1000 == 0 {
                data.push(r.byte());
            }
        }
        for i in 0..20_000u32 {
            data.extend_from_slice(&(i.wrapping_mul(0x01010101)).to_le_bytes());
        }
        // RGB gradient (span 4 with alpha).
        for y in 0..64u32 {
            for x in 0..256u32 {
                data.extend_from_slice(&[x as u8, y as u8 * 4, (x + y) as u8, 0xff]);
            }
        }
        let c = roundtrip(&data);
        assert!(
            c < data.len() / 4,
            "delta data should compress, got {c} / {}",
            data.len()
        );
    }

    #[test]
    fn delta_items_are_actually_emitted() {
        // Guard against the test silently not exercising delta paths: with
        // delta disabled this data compresses far worse.
        let data: Vec<u8> = (0..100_000u32)
            .flat_map(|i| ((i * 7) as u16).to_le_bytes())
            .collect();
        let with = compress(&data, &Options::default()).len();
        let without = compress(
            &data,
            &Options {
                delta: false,
                ..Default::default()
            },
        )
        .len();
        assert!(with * 4 < without, "with={with} without={without}");
        roundtrip(&data);
    }

    #[test]
    fn x86_code_is_filtered_and_restored() {
        let mut r = Rng(11);
        let mut data = Vec::new();
        for k in 0..30_000u32 {
            match r.next() % 5 {
                0 => {
                    data.push(0xE8);
                    data.extend_from_slice(
                        &(0x4000u32.wrapping_sub(data.len() as u32)).to_le_bytes(),
                    );
                }
                1 => {
                    data.extend_from_slice(&[0x48, 0x8D, 0x05]);
                    data.extend_from_slice(
                        &(0x9000u32.wrapping_sub(data.len() as u32)).to_le_bytes(),
                    );
                }
                2 => {
                    data.extend_from_slice(&[0xFF, 0x15]);
                    data.extend_from_slice(
                        &(0x9100u32.wrapping_sub(data.len() as u32)).to_le_bytes(),
                    );
                }
                3 => {
                    data.extend_from_slice(&[0xF0, 0x83, 0x05]);
                    data.extend_from_slice(&(k & 0xff).to_le_bytes());
                    data.push(1);
                }
                _ => data.extend_from_slice(&[0x55, 0x89, 0xE5, 0xE9, r.byte(), 0, 0, 0]),
            }
        }
        // Confirm the filter really changes this data, then round-trip.
        let mut f = data.clone();
        super::x86::x86_filter(&mut f, &mut Box::new([0i32; 65536]), false);
        assert_ne!(f, data);
        roundtrip(&data);
    }

    #[test]
    fn long_matches_and_far_offsets() {
        // > 2^16 run (uses the 16- and 30-extra-bit length slots region) and
        // a repeat of a 1 MiB-old random chunk (large offset slots).
        let mut r = Rng(5);
        let chunk: Vec<u8> = (0..50_000).map(|_| r.byte()).collect();
        let mut data = chunk.clone();
        data.extend(std::iter::repeat(0x41).take(200_000));
        data.extend((0..1_100_000).map(|_| r.byte()));
        data.extend_from_slice(&chunk);
        roundtrip(&data);
    }

    #[test]
    fn decompressor_is_reusable() {
        let mut dec = Decompressor::new();
        let mut r = Rng(9);
        for n in [1000usize, 5, 70_000, 2, 0, 33] {
            let data: Vec<u8> = (0..n)
                .map(|i| if i % 3 == 0 { r.byte() } else { (i / 3) as u8 })
                .collect();
            let c = compress(&data, &Options::default());
            let mut out = vec![0u8; n];
            dec.decompress(&c, &mut out).unwrap();
            assert_eq!(out, data);
        }
    }

    #[test]
    fn every_item_type_and_rep_slot_is_exercised() {
        let mut r = Rng(17);
        let mut data = Vec::new();
        for i in 0..40_000u32 {
            data.extend_from_slice(&((i * 5) as u16).to_le_bytes());
            if r.next() % 50 == 0 {
                data.extend_from_slice(b"some text, some text");
            }
            if r.next() % 70 == 0 {
                data.push(r.byte());
            }
        }
        let (c, stats) = compress_with_stats(
            &data,
            &Options {
                max_len: 9,
                ..Default::default()
            },
        );
        assert!(stats.iter().all(|&n| n > 50), "stats {stats:?}");
        assert_eq!(decompress_to_vec(&c, data.len()).unwrap(), data);
    }

    #[test]
    fn garbage_input_never_panics() {
        let mut r = Rng(42);
        for iter in 0..3000 {
            let in_len = 4 + 2 * (r.next() % 300) as usize;
            let input: Vec<u8> = (0..in_len).map(|_| r.byte()).collect();
            let out_len = (r.next() % 4000) as usize;
            let mut out = vec![0u8; out_len];
            let _ = decompress(&input, &mut out); // Ok or Err, but no panic
            let _ = iter;
        }
    }

    #[test]
    fn bit_flips_never_panic() {
        let mut r = Rng(43);
        let data: Vec<u8> = (0..20_000u32)
            .map(|i| (i % 251) as u8 ^ (i / 97) as u8)
            .collect();
        let good = compress(&data, &Options::default());
        for _ in 0..2000 {
            let mut c = good.clone();
            let i = (r.next() as usize) % c.len();
            c[i] ^= 1 << (r.next() % 8);
            let mut out = vec![0u8; data.len()];
            let _ = decompress(&c, &mut out);
        }
    }
}
