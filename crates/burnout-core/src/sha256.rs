//! SHA-256, which says whether a drive holds the image that went onto it.
//!
//! This is written out here rather than taken from a crate, so that
//! `burnout-core` keeps no dependency at all. The algorithm is fixed, it is
//! about a hundred lines, and the published vectors in the tests below prove
//! this copy of it. A hash that passes those vectors is the hash.

use std::fmt;

/// The result of a hash, as thirty-two bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digest([u8; 32]);

impl Digest {
    /// The bytes of the digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Digest {
    /// Lower case hexadecimal, the form that every publisher prints.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The hash of one buffer.
pub fn sha256(bytes: &[u8]) -> Digest {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finish()
}

/// A hash that takes the bytes a piece at a time.
///
/// The write path reads the source in blocks, so it never holds the whole
/// image, and this takes each block as it goes past.
#[derive(Clone, Debug)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    /// The number of bytes fed in, which the padding needs at the end.
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: INITIAL,
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Take the next bytes of the message.
    pub fn update(&mut self, bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        self.absorb(bytes);
    }

    /// Finish the message and give the digest.
    pub fn finish(mut self) -> Digest {
        // The length goes in as a count of bits, and it counts the message
        // and not the padding, so take it before the padding goes in.
        let bits = self.length.wrapping_mul(8);
        self.absorb(&[0x80]);
        while self.buffered != 56 {
            self.absorb(&[0]);
        }
        self.absorb(&bits.to_be_bytes());
        debug_assert_eq!(self.buffered, 0);

        let mut out = [0u8; 32];
        for (slot, word) in out.chunks_exact_mut(4).zip(self.state.iter()) {
            slot.copy_from_slice(&word.to_be_bytes());
        }
        Digest(out)
    }

    /// Feed bytes through the buffer without counting them.
    ///
    /// The padding uses this. Counting the padding would change the length
    /// that the padding itself carries.
    fn absorb(&mut self, mut bytes: &[u8]) {
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&bytes[..take]);
            self.buffered += take;
            bytes = &bytes[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                compress(&mut self.state, &block);
                self.buffered = 0;
            }
        }
        while bytes.len() >= 64 {
            let (block, rest) = bytes.split_at(64);
            let mut whole = [0u8; 64];
            whole.copy_from_slice(block);
            compress(&mut self.state, &whole);
            bytes = rest;
        }
        if !bytes.is_empty() {
            self.buffer[..bytes.len()].copy_from_slice(bytes);
            self.buffered = bytes.len();
        }
    }
}

/// The first thirty-two bits of the fractional parts of the square roots of
/// the first eight prime numbers.
const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// The first thirty-two bits of the fractional parts of the cube roots of the
/// first sixty-four prime numbers.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// One block of sixty-four bytes into the state.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, chunk) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(chunk.try_into().expect("a chunk of four is four bytes"));
    }
    for i in 16..64 {
        let x = w[i - 15];
        let y = w[i - 2];
        let s0 = x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3);
        let s1 = y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, word) in K.iter().zip(w.iter()) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ ((!e) & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(choice)
            .wrapping_add(*k)
            .wrapping_add(*word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(majority);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(bytes: &[u8]) -> String {
        sha256(bytes).to_string()
    }

    #[test]
    fn the_empty_message_gives_the_published_digest() {
        // An empty message is one padding block and nothing else, so this
        // fails first when the padding is wrong.
        assert_eq!(
            hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_three_letter_message_gives_the_published_digest() {
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_message_of_fifty_six_bytes_gives_the_published_digest() {
        // Fifty-six bytes is the boundary. The length does not fit beside the
        // message in one block, so the padding takes a second block. A
        // version that pads in one block passes every shorter test and fails
        // here.
        let message = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(message.len(), 56);
        assert_eq!(
            hex_of(message),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn a_message_of_a_hundred_and_twelve_bytes_gives_the_published_digest() {
        let message = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                        ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(message.len(), 112);
        assert_eq!(
            hex_of(message),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn a_million_letters_give_the_published_digest() {
        // This one crosses the length that a single block can count, and it
        // is the vector that catches a counter kept in the wrong units.
        let message = vec![b'a'; 1_000_000];
        assert_eq!(
            hex_of(&message),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn bytes_fed_in_pieces_give_the_same_digest_as_one_call() {
        // The write path never holds the whole image, so this is the way the
        // hash is used in the product.
        let message: Vec<u8> = (0u32..5000).map(|i| (i % 251) as u8).collect();
        let whole = sha256(&message);

        let mut piecewise = Sha256::new();
        let mut rest = &message[..];
        // Sizes that are not a factor of the block, so the buffer carries a
        // partial block from one call to the next.
        for size in [1usize, 63, 64, 65, 127, 1, 500].iter().cycle() {
            if rest.is_empty() {
                break;
            }
            let take = (*size).min(rest.len());
            piecewise.update(&rest[..take]);
            rest = &rest[take..];
        }
        assert_eq!(piecewise.finish(), whole);
    }

    #[test]
    fn a_digest_prints_as_sixty_four_lower_case_characters() {
        let text = sha256(b"abc").to_string();
        assert_eq!(text.len(), 64);
        assert!(text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn two_messages_that_differ_in_one_bit_give_different_digests() {
        assert_ne!(sha256(b"burnout"), sha256(b"burnoun"));
    }
}
