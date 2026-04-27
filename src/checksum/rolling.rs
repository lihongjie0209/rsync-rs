#![allow(dead_code)]

/// Compute the rsync rolling checksum, exactly matching C's `get_checksum1()`.
///
/// CRITICAL: every byte is interpreted as `i8` (signed char), matching the C cast
/// `schar *buf = (schar *)buf1`.  Byte values ≥ 128 become negative numbers in
/// the accumulation — this is protocol-critical.
///
/// CHAR_OFFSET is 0 so all `+ CHAR_OFFSET` terms vanish.
pub fn checksum1(data: &[u8]) -> u32 {
    let len = data.len();
    let mut s1: u32 = 0;
    let mut s2: u32 = 0;
    let mut i = 0usize;

    // Unrolled 4-byte fast path.
    // C: `for (i = 0; i < (len-4); i += 4)` — with int32 semantics the loop body
    // never runs when len ≤ 4.  `saturating_sub` gives the same guard.
    let fast_end = len.saturating_sub(4);
    while i < fast_end {
        let b0 = (data[i]     as i8) as u32;
        let b1 = (data[i + 1] as i8) as u32;
        let b2 = (data[i + 2] as i8) as u32;
        let b3 = (data[i + 3] as i8) as u32;
        // s2 += 4*(s1 + buf[i]) + 3*buf[i+1] + 2*buf[i+2] + buf[i+3]
        s2 = s2.wrapping_add(
            4u32.wrapping_mul(s1.wrapping_add(b0))
                .wrapping_add(3u32.wrapping_mul(b1))
                .wrapping_add(2u32.wrapping_mul(b2))
                .wrapping_add(b3),
        );
        // s1 += buf[i+0] + buf[i+1] + buf[i+2] + buf[i+3]
        s1 = s1
            .wrapping_add(b0)
            .wrapping_add(b1)
            .wrapping_add(b2)
            .wrapping_add(b3);
        i += 4;
    }

    // Scalar tail — at most 4 bytes remain.
    while i < len {
        let b = (data[i] as i8) as u32;
        s1 = s1.wrapping_add(b);
        s2 = s2.wrapping_add(s1);
        i += 1;
    }

    (s1 & 0xffff) | (s2 << 16)
}

/// Incremental rolling-checksum state.
///
/// Mirrors the `s1`/`s2` accumulators maintained in `hash_search` (match.c).
/// After [`init`] the window is fixed-size; [`roll`] slides it by one byte.
pub struct RollingChecksum {
    pub s1: u32,
    pub s2: u32,
    /// Number of bytes in the current window.
    pub count: u32,
}

impl Default for RollingChecksum {
    fn default() -> Self {
        Self::new()
    }
}

impl RollingChecksum {
    pub fn new() -> Self {
        RollingChecksum { s1: 0, s2: 0, count: 0 }
    }

    pub fn reset(&mut self) {
        self.s1 = 0;
        self.s2 = 0;
        self.count = 0;
    }

    /// Initialise from a complete block — equivalent to calling `get_checksum1`
    /// and decomposing the result.
    ///
    /// C: `sum = get_checksum1(map, k); s1 = sum & 0xFFFF; s2 = sum >> 16;`
    pub fn init(&mut self, data: &[u8]) {
        let sum = checksum1(data);
        self.s1 = sum & 0xffff;
        self.s2 = sum >> 16;
        self.count = data.len() as u32;
    }

    /// Slide the window: remove `old_byte` (departing) and add `new_byte` (arriving).
    ///
    /// Matches the rolling update in `hash_search` (match.c, CHAR_OFFSET = 0):
    /// ```c
    /// s1 -= map[0];          // signed-char byte leaving the window
    /// s2 -= k * map[0];
    /// s1 += map[k];          // signed-char byte entering the window
    /// s2 += s1;
    /// ```
    /// Both bytes are sign-extended to `i8` then reinterpreted as `u32` before
    /// wrapping arithmetic — e.g. `0x80 as i8 as u32 == 0xFFFFFF80`.
    pub fn roll(&mut self, old_byte: u8, new_byte: u8) {
        let old  = (old_byte as i8) as u32;
        let new_b = (new_byte as i8) as u32;
        let k = self.count;

        self.s1 = self.s1.wrapping_sub(old);
        self.s2 = self.s2.wrapping_sub(k.wrapping_mul(old));
        self.s1 = self.s1.wrapping_add(new_b);
        self.s2 = self.s2.wrapping_add(self.s1);
    }

    /// Return the packed 32-bit checksum: `(s1 & 0xffff) | (s2 << 16)`.
    pub fn value(&self) -> u32 {
        (self.s1 & 0xffff) | (self.s2 << 16)
    }

    /// Return `(s1 & 0xffff, s2 & 0xffff)` as a pair of 16-bit values.
    pub fn s1_s2(&self) -> (u16, u16) {
        ((self.s1 & 0xffff) as u16, (self.s2 & 0xffff) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum1_empty() {
        assert_eq!(checksum1(b""), 0);
    }

    #[test]
    fn checksum1_single_zero() {
        // len=1 so fast path never runs; slow path: s1=0, s2=0.
        assert_eq!(checksum1(&[0u8]), 0);
    }

    /// Manually verified against the C implementation:
    ///   "hello world" → s1=1116 (0x045C), s2=6656 (0x1A00) → 0x1A00_045C
    #[test]
    fn checksum1_hello_world() {
        assert_eq!(checksum1(b"hello world"), 0x1A00_045C);
    }

    /// Bytes ≥ 128 are treated as i8 (negative).
    /// 0x80 as i8 = −128.  Slow path only:
    ///   s1 = 0.wrapping_add(0xFFFFFF80) = 0xFFFFFF80
    ///   s2 = 0.wrapping_add(0xFFFFFF80) = 0xFFFFFF80
    ///   value = 0xFF80 | 0xFF80_0000 = 0xFF80_FF80
    #[test]
    fn checksum1_high_byte() {
        assert_eq!(checksum1(&[0x80u8]), 0xFF80_FF80);
    }

    /// A block of exactly 4 bytes — the fast loop condition `i < len-4 = 0` is
    /// false from the start, so the entire block goes through the slow path.
    #[test]
    fn checksum1_four_bytes() {
        let data = [1u8, 2, 3, 4];
        // Slow path: s1=1→3→6→10, s2=1→4→10→20
        // value = (10 & 0xffff) | (20 << 16) = 0x000A | 0x0014_0000 = 0x0014_000A
        assert_eq!(checksum1(&data), 0x0014_000A);
    }

    /// Fast path starts when len > 4.  For len=5 the fast loop runs once (i=0..3)
    /// and the slow path handles the last byte.
    #[test]
    fn checksum1_five_bytes() {
        let data = [1u8, 2, 3, 4, 5];
        // Fast (i=0): s2 += 4*(0+1)+3*2+2*3+4 = 4+6+6+4=20; s1 += 1+2+3+4=10
        // Slow (i=4): b=5; s1=15; s2=20+15=35
        // value = 15 | (35 << 16) = 0x000F | 0x0023_0000 = 0x0023_000F
        assert_eq!(checksum1(&data), 0x0023_000F);
    }

    #[test]
    fn rolling_init_matches_checksum1() {
        let data = b"hello world";
        let mut rc = RollingChecksum::new();
        rc.init(data);
        assert_eq!(rc.value(), checksum1(data));
    }

    #[test]
    fn rolling_roll_ascii() {
        // Roll "hello world" → "ello world!" (remove 'h'=104, add '!'=33).
        // Both bytes are ASCII so no sign-extension issues.
        let window = b"hello world";
        let mut rc = RollingChecksum::new();
        rc.init(window);
        rc.roll(b'h', b'!');
        assert_eq!(rc.value(), checksum1(b"ello world!"));
    }

    #[test]
    fn rolling_roll_consecutive() {
        // Sliding window of width 3 over [1,2,3,4,5]
        let data = [1u8, 2, 3, 4, 5];
        let k = 3;
        let mut rc = RollingChecksum::new();
        rc.init(&data[0..k]);
        // slide once: remove data[0]=1, add data[3]=4
        rc.roll(data[0], data[3]);
        assert_eq!(rc.value(), checksum1(&data[1..4]));
        // slide again: remove data[1]=2, add data[4]=5
        rc.roll(data[1], data[4]);
        assert_eq!(rc.value(), checksum1(&data[2..5]));
    }

    #[test]
    fn rolling_count_preserved() {
        let data = b"abcde";
        let mut rc = RollingChecksum::new();
        rc.init(data);
        assert_eq!(rc.count, 5);
        rc.roll(b'a', b'f');
        assert_eq!(rc.count, 5); // count never changes after init
    }

    #[test]
    fn rolling_reset() {
        let mut rc = RollingChecksum::new();
        rc.init(b"hello");
        rc.reset();
        assert_eq!(rc.s1, 0);
        assert_eq!(rc.s2, 0);
        assert_eq!(rc.count, 0);
        assert_eq!(rc.value(), 0);
    }
}
