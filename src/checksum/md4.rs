#![allow(dead_code)]

//! Pure-Rust MD4 matching rsync's `lib/mdfour.c` — including protocol quirks.
//!
//! rsync uses three distinct MD4 behaviours depending on the protocol version:
//!
//! | Variant       | Protocol | Bit-count width | Finalize when tail=0? |
//! |---------------|----------|-----------------|----------------------|
//! | ARCHAIC (1)   | < 21     | 32-bit          | **no** (bug)         |
//! | BUSTED  (2)   | 21–26    | 32-bit          | **no** (bug)         |
//! | OLD     (3)   | 27–29    | 64-bit (RFC1320)| yes                  |
//! | STANDARD(4)   | 30+      | 64-bit (RFC1320)| yes                  |
//!
//! `md4_classic` handles ARCHAIC and BUSTED (custom implementation required
//! because the md-4 crate cannot skip finalisation or use a 32-bit bit-length).
//!
//! `md4_modern` wraps the `md-4` crate for OLD and STANDARD (standard RFC 1320).

use md4::Md4;
use digest::Digest;

// ---------------------------------------------------------------------------
// Helper functions (F, G, H) and the compression function
// ---------------------------------------------------------------------------

#[inline(always)]
fn ff(x: u32, y: u32, z: u32) -> u32 { (x & y) | (!x & z) }
#[inline(always)]
fn gg(x: u32, y: u32, z: u32) -> u32 { (x & y) | (x & z) | (y & z) }
#[inline(always)]
fn hh(x: u32, y: u32, z: u32) -> u32 { x ^ y ^ z }

/// Internal MD4 state — mirrors `md_context` in mdfour.c.
struct Md4State {
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    /// Total bits processed so far, lower 32 bits (totalN).
    total_n: u32,
    /// Total bits processed so far, upper 32 bits (totalN2).
    total_n2: u32,
}

impl Md4State {
    fn new() -> Self {
        Self {
            a: 0x67452301,
            b: 0xefcdab89,
            c: 0x98badcfe,
            d: 0x10325476,
            total_n: 0,
            total_n2: 0,
        }
    }

    /// Apply one round of MD4 compression to a single 64-byte block.
    /// Does NOT update the bit counter — call `update_block` for that.
    fn compress(&mut self, block: &[u8; 64]) {
        // Read 16 little-endian u32s (copy64 in C).
        let m: [u32; 16] = core::array::from_fn(|i| {
            u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ])
        });

        let (mut a, mut b, mut c, mut d) = (self.a, self.b, self.c, self.d);
        let (aa, bb, cc, dd) = (a, b, c, d);

        // ROUND1(a,b,c,d,k,s) = a = lshift((a + F(b,c,d) + M[k]) & MASK32, s)
        // lshift == rotate_left for u32.
        macro_rules! r1 {
            ($a:ident,$b:ident,$c:ident,$d:ident,$k:expr,$s:expr) => {
                $a = $a
                    .wrapping_add(ff($b, $c, $d))
                    .wrapping_add(m[$k])
                    .rotate_left($s);
            };
        }
        // ROUND2 adds constant 0x5A827999
        macro_rules! r2 {
            ($a:ident,$b:ident,$c:ident,$d:ident,$k:expr,$s:expr) => {
                $a = $a
                    .wrapping_add(gg($b, $c, $d))
                    .wrapping_add(m[$k])
                    .wrapping_add(0x5A827999)
                    .rotate_left($s);
            };
        }
        // ROUND3 adds constant 0x6ED9EBA1
        macro_rules! r3 {
            ($a:ident,$b:ident,$c:ident,$d:ident,$k:expr,$s:expr) => {
                $a = $a
                    .wrapping_add(hh($b, $c, $d))
                    .wrapping_add(m[$k])
                    .wrapping_add(0x6ED9EBA1)
                    .rotate_left($s);
            };
        }

        // Round 1
        r1!(a, b, c, d,  0,  3);  r1!(d, a, b, c,  1,  7);
        r1!(c, d, a, b,  2, 11);  r1!(b, c, d, a,  3, 19);
        r1!(a, b, c, d,  4,  3);  r1!(d, a, b, c,  5,  7);
        r1!(c, d, a, b,  6, 11);  r1!(b, c, d, a,  7, 19);
        r1!(a, b, c, d,  8,  3);  r1!(d, a, b, c,  9,  7);
        r1!(c, d, a, b, 10, 11);  r1!(b, c, d, a, 11, 19);
        r1!(a, b, c, d, 12,  3);  r1!(d, a, b, c, 13,  7);
        r1!(c, d, a, b, 14, 11);  r1!(b, c, d, a, 15, 19);

        // Round 2
        r2!(a, b, c, d,  0,  3);  r2!(d, a, b, c,  4,  5);
        r2!(c, d, a, b,  8,  9);  r2!(b, c, d, a, 12, 13);
        r2!(a, b, c, d,  1,  3);  r2!(d, a, b, c,  5,  5);
        r2!(c, d, a, b,  9,  9);  r2!(b, c, d, a, 13, 13);
        r2!(a, b, c, d,  2,  3);  r2!(d, a, b, c,  6,  5);
        r2!(c, d, a, b, 10,  9);  r2!(b, c, d, a, 14, 13);
        r2!(a, b, c, d,  3,  3);  r2!(d, a, b, c,  7,  5);
        r2!(c, d, a, b, 11,  9);  r2!(b, c, d, a, 15, 13);

        // Round 3
        r3!(a, b, c, d,  0,  3);  r3!(d, a, b, c,  8,  9);
        r3!(c, d, a, b,  4, 11);  r3!(b, c, d, a, 12, 15);
        r3!(a, b, c, d,  2,  3);  r3!(d, a, b, c, 10,  9);
        r3!(c, d, a, b,  6, 11);  r3!(b, c, d, a, 14, 15);
        r3!(a, b, c, d,  1,  3);  r3!(d, a, b, c,  9,  9);
        r3!(c, d, a, b,  5, 11);  r3!(b, c, d, a, 13, 15);
        r3!(a, b, c, d,  3,  3);  r3!(d, a, b, c, 11,  9);
        r3!(c, d, a, b,  7, 11);  r3!(b, c, d, a, 15, 15);

        self.a = a.wrapping_add(aa);
        self.b = b.wrapping_add(bb);
        self.c = c.wrapping_add(cc);
        self.d = d.wrapping_add(dd);
    }

    /// Process a full 64-byte block and update the bit counter.
    fn update_block(&mut self, block: &[u8; 64]) {
        self.compress(block);
        // m->totalN += 64 << 3 = 512; if (m->totalN < 512) m->totalN2++;
        let new_n = self.total_n.wrapping_add(512);
        if new_n < 512 {
            self.total_n2 = self.total_n2.wrapping_add(1);
        }
        self.total_n = new_n;
    }

    /// Return the raw A,B,C,D state as a 16-byte digest **without** finalisation.
    ///
    /// Used for the CSUM_MD4_BUSTED/ARCHAIC bug: when the total length is a
    /// multiple of 64, `mdfour_update(ctx, ptr, 0)` is never called, so the
    /// context is never padded.  `mdfour_result` then reads the bare state.
    fn result_raw(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.a.to_le_bytes());
        out[4..8].copy_from_slice(&self.b.to_le_bytes());
        out[8..12].copy_from_slice(&self.c.to_le_bytes());
        out[12..16].copy_from_slice(&self.d.to_le_bytes());
        out
    }

    /// Finalise: apply Merkle-Damgård padding to `remaining` and return digest.
    ///
    /// `use_64bit_count` — `true` for protocol ≥ 27 (full RFC 1320 bit-length);
    ///                     `false` for protocol < 27 (only lower 32 bits written).
    ///
    /// Mirrors `mdfour_tail` in mdfour.c exactly.
    fn finalize_tail(&mut self, remaining: &[u8], use_64bit_count: bool) -> [u8; 16] {
        let length = remaining.len() as u32;

        // m->totalN += length << 3; if (m->totalN < (length<<3)) m->totalN2++;
        // m->totalN2 += length >> 29;
        let bits = length.wrapping_shl(3);
        let new_n = self.total_n.wrapping_add(bits);
        if new_n < bits {
            self.total_n2 = self.total_n2.wrapping_add(1);
        }
        self.total_n = new_n;
        self.total_n2 = self.total_n2.wrapping_add(length.wrapping_shr(29));

        // Build the padded buffer (up to 128 bytes).
        let mut buf = [0u8; 128];
        if !remaining.is_empty() {
            buf[..remaining.len()].copy_from_slice(remaining);
        }
        buf[length as usize] = 0x80; // append bit '1'

        if length <= 55 {
            // Fits in one 64-byte block.
            buf[56..60].copy_from_slice(&self.total_n.to_le_bytes());
            if use_64bit_count {
                buf[60..64].copy_from_slice(&self.total_n2.to_le_bytes());
            }
            self.compress(buf[0..64].try_into().unwrap());
        } else {
            // Needs two 64-byte blocks; bit-length goes in the second block.
            buf[120..124].copy_from_slice(&self.total_n.to_le_bytes());
            if use_64bit_count {
                buf[124..128].copy_from_slice(&self.total_n2.to_le_bytes());
            }
            self.compress(buf[0..64].try_into().unwrap());
            self.compress(buf[64..128].try_into().unwrap());
        }

        self.result_raw()
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute rsync MD4 for **classic** protocols (CSUM_MD4_ARCHAIC / CSUM_MD4_BUSTED,
/// protocol < 27).
///
/// Replicates two quirks from mdfour.c:
/// 1. **32-bit bit-length** in the padding (upper 32 bits of the bit counter
///    are not written, regardless of the actual data size).
/// 2. **Skip finalisation** when `data_with_seed.len() % 64 == 0` — the
///    `mdfour_update(ctx, ptr, 0)` call is omitted in `get_checksum2`, leaving
///    the context un-padded.  `mdfour_result` then returns the raw state.
///
/// The caller must already have appended any seed bytes to `data_with_seed`.
pub(crate) fn md4_classic(data_with_seed: &[u8]) -> [u8; 16] {
    let len = data_with_seed.len();
    let mut state = Md4State::new();
    let mut i = 0;

    while i + 64 <= len {
        state.update_block(data_with_seed[i..i + 64].try_into().unwrap());
        i += 64;
    }

    let remainder = &data_with_seed[i..];
    if !remainder.is_empty() {
        // There are leftover bytes — always finalize.
        state.finalize_tail(remainder, false /* 32-bit count */)
    } else {
        // len is a multiple of 64: replicate the BUSTED/ARCHAIC bug —
        // return raw state without any MD4 padding.
        state.result_raw()
    }
}

/// Compute rsync MD4 for **modern** protocols (CSUM_MD4_OLD / CSUM_MD4,
/// protocol ≥ 27).
///
/// These variants use standard RFC 1320 MD4 (64-bit bit-length, always
/// finalise).  We delegate to the `md-4` crate which implements exactly this.
///
/// The caller must already have appended any seed bytes to `data_with_seed`.
pub(crate) fn md4_modern(data_with_seed: &[u8]) -> [u8; 16] {
    let mut hasher = Md4::new();
    hasher.update(data_with_seed);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Sanity: md4_modern must produce standard RFC 1320 MD4.
    // Known vector: MD4("") = 31d6cfe0d16ae931b73c59d7e0c089c0
    // -----------------------------------------------------------------------
    #[test]
    fn md4_modern_empty() {
        let got = md4_modern(b"");
        let expected: [u8; 16] = [
            0x31, 0xd6, 0xcf, 0xe0, 0xd1, 0x6a, 0xe9, 0x31,
            0xb7, 0x3c, 0x59, 0xd7, 0xe0, 0xc0, 0x89, 0xc0,
        ];
        assert_eq!(got, expected);
    }

    // Known vector: MD4("a") = bde52cb31de33e46245e05fbdbd6fb24
    #[test]
    fn md4_modern_a() {
        let got = md4_modern(b"a");
        let expected: [u8; 16] = [
            0xbd, 0xe5, 0x2c, 0xb3, 0x1d, 0xe3, 0x3e, 0x46,
            0x24, 0x5e, 0x05, 0xfb, 0xdb, 0xd6, 0xfb, 0x24,
        ];
        assert_eq!(got, expected);
    }

    // Known vector: MD4("abc") = a448017aaf21d8525fc10ae87aa6729d
    #[test]
    fn md4_modern_abc() {
        let got = md4_modern(b"abc");
        let expected: [u8; 16] = [
            0xa4, 0x48, 0x01, 0x7a, 0xaf, 0x21, 0xd8, 0x52,
            0x5f, 0xc1, 0x0a, 0xe8, 0x7a, 0xa6, 0x72, 0x9d,
        ];
        assert_eq!(got, expected);
    }

    // For data whose length is NOT a multiple of 64, md4_classic and md4_modern
    // should differ only in bit-count width.  For short inputs totalN2 == 0, so
    // both functions must produce identical output.
    //
    // NOTE: b"" (len=0) is excluded because 0 % 64 == 0 — the BUSTED bug
    // applies and classic returns the raw init vector without any hashing.
    #[test]
    fn classic_and_modern_agree_for_short_data() {
        for data in &[b"abc".as_ref(), b"hello world", b"x"] {
            assert_eq!(
                md4_classic(data),
                md4_modern(data),
                "mismatch for {:?}",
                data
            );
        }
    }

    // For the empty string, classic returns raw IV (0 % 64 == 0 triggers the bug).
    #[test]
    fn classic_empty_returns_raw_iv() {
        let got = md4_classic(b"");
        // Initial MD4 state A,B,C,D in little-endian:
        let expected: [u8; 16] = [
            0x01, 0x23, 0x45, 0x67,  // A = 0x67452301
            0x89, 0xab, 0xcd, 0xef,  // B = 0xefcdab89
            0xfe, 0xdc, 0xba, 0x98,  // C = 0x98badcfe
            0x76, 0x54, 0x32, 0x10,  // D = 0x10325476
        ];
        assert_eq!(got, expected);
        // And it must differ from the proper RFC 1320 hash.
        assert_ne!(got, md4_modern(b""));
    }

    // For exactly-64-byte data, md4_classic returns raw state (bug); md4_modern
    // returns a proper RFC 1320 hash — they must DIFFER.
    #[test]
    fn classic_skips_finalize_for_64_byte_input() {
        let data = [0x41u8; 64]; // 64 × 'A'
        let classic = md4_classic(&data);
        let modern  = md4_modern(&data);
        assert_ne!(classic, modern, "classic should NOT equal modern for 64-byte input");
    }

    // For 65-byte data, classic finalises normally (remainder = 1 byte > 0).
    #[test]
    fn classic_finalizes_when_remainder_nonzero() {
        let data = [0x41u8; 65]; // 64 + 1 byte
        // Classic finalises with 32-bit count; for this size totalN2 == 0,
        // so the result equals modern.
        assert_eq!(md4_classic(&data), md4_modern(&data));
    }
}
