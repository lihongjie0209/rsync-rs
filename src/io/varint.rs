#![allow(dead_code)]

use std::io::{Read, Write};

/// Maps `first_byte / 4` to number of additional bytes to read.
/// Matches rsync's `int_byte_extra[64]` table in io.c exactly.
static INT_BYTE_EXTRA: [u8; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x00-0x3F)/4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x40-0x7F)/4
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // (0x80-0xBF)/4
    2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6, // (0xC0-0xFF)/4
];

// ── Single byte ───────────────────────────────────────────────────────────

pub fn read_byte<R: Read>(r: &mut R) -> anyhow::Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

pub fn write_byte<W: Write>(w: &mut W, x: u8) -> anyhow::Result<()> {
    w.write_all(&[x])?;
    Ok(())
}

// ── 2-byte little-endian unsigned ─────────────────────────────────────────

pub fn read_shortint<R: Read>(r: &mut R) -> anyhow::Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

pub fn write_shortint<W: Write>(w: &mut W, x: u16) -> anyhow::Result<()> {
    w.write_all(&x.to_le_bytes())?;
    Ok(())
}

// ── 4-byte little-endian int32 ────────────────────────────────────────────

pub fn read_int<R: Read>(r: &mut R) -> anyhow::Result<i32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

pub fn write_int<W: Write>(w: &mut W, x: i32) -> anyhow::Result<()> {
    w.write_all(&x.to_le_bytes())?;
    Ok(())
}

// ── 4-byte or 12-byte int64 (rsync "longint") ─────────────────────────────
//
// Wire format:
//   If 0 <= x <= 0x7FFFFFFF:  4 bytes (little-endian int32).
//   Otherwise:                4 bytes 0xFFFFFFFF sentinel + 8 bytes LE int64.

pub fn read_longint<R: Read>(r: &mut R) -> anyhow::Result<i64> {
    let num = read_int(r)?;
    if num != -1i32 {
        return Ok(num as i64);
    }
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(i64::from_le_bytes(buf))
}

pub fn write_longint<W: Write>(w: &mut W, x: i64) -> anyhow::Result<()> {
    if x >= 0 && x <= i32::MAX as i64 {
        w.write_all(&(x as i32).to_le_bytes())?;
    } else {
        w.write_all(&[0xFF, 0xFF, 0xFF, 0xFF])?;
        w.write_all(&x.to_le_bytes())?;
    }
    Ok(())
}

// ── varint (variable-length int32) ────────────────────────────────────────
//
// Encoding:
//   Marker byte ch encodes how many extra bytes follow (via INT_BYTE_EXTRA).
//   Extra bytes come first (little-endian), then the masked bits of ch fill
//   the highest position.  The result is assembled as a LE i32.
//
//   ch range   extra  bit    value range
//   0x00-0x7F    0    —      0..127          (just ch)
//   0x80-0xBF    1   0x80   0..16383         (1 byte + 7 bits from ch)
//   0xC0-0xDF    2   0x40   0..4194303       (2 bytes + 6 bits from ch)
//   0xE0-0xEF    3   0x20   0..1073741823    (3 bytes + 5 bits from ch)
//   0xF0-0xF7    4   0x10   full 29-bit      (4 bytes + 4 bits from ch)
//   0xF8-0xFB    5   0x08   …                (only used by varlong)
//   0xFC-0xFF    6   0x04   …                (only used by varlong)

pub fn read_varint<R: Read>(r: &mut R) -> anyhow::Result<i32> {
    let ch = read_byte(r)?;
    let extra = INT_BYTE_EXTRA[(ch / 4) as usize] as usize;
    let mut buf = [0u8; 5]; // 4 bytes for i32 + 1 overflow slot
    if extra > 0 {
        let bit: u8 = 1u8 << (8 - extra); // high-bit mask for the ch prefix
        r.read_exact(&mut buf[..extra])?;
        buf[extra] = ch & (bit - 1); // strip prefix bits from ch, place as high byte
    } else {
        buf[0] = ch;
    }
    Ok(i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
}

pub fn write_varint<W: Write>(w: &mut W, x: i32) -> anyhow::Result<()> {
    let mut b = [0u8; 5];
    // Place LE bytes of x at b[1..5], leaving b[0] for the marker.
    let xb = x.to_le_bytes();
    b[1] = xb[0];
    b[2] = xb[1];
    b[3] = xb[2];
    b[4] = xb[3];

    // Find the highest non-zero byte position (1-indexed in b[]).
    let mut cnt: usize = 4;
    while cnt > 1 && b[cnt] == 0 {
        cnt -= 1;
    }

    // Compute the threshold bit for this byte count.
    // bit = 1 << (8 - cnt).  Values < bit fit in the remaining bits of the marker.
    let bit: u8 = 1u8 << (8 - cnt);

    if b[cnt] >= bit {
        // b[cnt] doesn't fit in the marker's spare bits; need an extra byte.
        cnt += 1;
        b[0] = !(bit - 1); // marker: all prefix bits set
    } else if cnt > 1 {
        // Embed b[cnt] into the marker byte, set the prefix bits.
        b[0] = b[cnt] | !(bit.wrapping_mul(2).wrapping_sub(1));
    } else {
        // Single byte; value fits in 7 bits, no prefix.
        b[0] = b[1];
    }

    w.write_all(&b[..cnt])?;
    Ok(())
}

// ── varint30 (protocol >= 30 alias for varint) ────────────────────────────
//
// rsync's io.h delegates to read_varint / write_varint for protocol >= 30
// (which is our target).

pub fn read_varint30<R: Read>(r: &mut R) -> anyhow::Result<i32> {
    read_varint(r)
}

pub fn write_varint30<W: Write>(w: &mut W, x: i32) -> anyhow::Result<()> {
    write_varint(w, x)
}

// ── varlong (variable-length int64 with configurable minimum byte count) ──
//
// The `min_bytes` parameter specifies the minimum wire bytes (1..=8).
// In rsync, it is always 3 (file lengths) or 4 (timestamps).
//
// Wire layout (write side, b[0..cnt]):
//   b[0]     = marker byte
//   b[1..]   = LE bytes of x
//
// Wire layout (read side):
//   first `min_bytes` bytes  →  b2[0] is marker, b2[1..] are low data bytes
//   then `extra` more bytes  →  middle data bytes
//   masked bits of marker    →  high data byte

pub fn read_varlong<R: Read>(r: &mut R, min_bytes: u8) -> anyhow::Result<i64> {
    let mb = min_bytes as usize;
    let mut b2 = [0u8; 8];
    r.read_exact(&mut b2[..mb])?;

    let mut buf = [0u8; 9]; // 8 bytes for i64 + 1 overflow slot
    // b2[1..mb] are the low data bytes, placed at buf[0..mb-1].
    buf[..mb - 1].copy_from_slice(&b2[1..mb]);

    let ch = b2[0]; // marker byte
    let extra = INT_BYTE_EXTRA[(ch / 4) as usize] as usize;

    if extra > 0 {
        let bit: u8 = 1u8 << (8 - extra);
        // Read middle data bytes into buf[mb-1..mb-1+extra].
        r.read_exact(&mut buf[mb - 1..mb - 1 + extra])?;
        // Stripped marker bits form the high data byte.
        buf[mb + extra - 1] = ch & (bit - 1);
    } else {
        buf[mb - 1] = ch;
    }

    Ok(i64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]))
}

pub fn write_varlong<W: Write>(w: &mut W, x: i64, min_bytes: u8) -> anyhow::Result<()> {
    let mb = min_bytes as usize;
    let mut b = [0u8; 9]; // b[0] = marker, b[1..8] = LE bytes of x
    let xb = x.to_le_bytes();
    b[1..9].copy_from_slice(&xb);

    // Find the highest non-zero byte position, clamped to [mb, 8].
    let mut cnt: usize = 8;
    while cnt > mb && b[cnt] == 0 {
        cnt -= 1;
    }

    // bit = 1 << (7 + mb - cnt).  Safe: cnt <= 7+mb because cnt<=8 and mb>=1.
    let shift = 7 + mb - cnt;
    let bit: u8 = 1u8 << shift;

    if b[cnt] >= bit {
        cnt += 1;
        b[0] = !(bit - 1);
    } else if cnt > mb {
        b[0] = b[cnt] | !(bit.wrapping_mul(2).wrapping_sub(1));
    } else {
        b[0] = b[cnt];
    }

    w.write_all(&b[..cnt])?;
    Ok(())
}

// ── NDX delta-encoding (protocol 30+) ────────────────────────────────────
//
// rsync uses a stateful delta scheme so that consecutive file indices
// compress to 1 or 3 bytes.  State is kept in thread-local storage so
// callers don't need to pass it explicitly (matching C's static variables).
//
// Wire format (from C rsync io.c write_ndx / read_ndx):
//
//   NDX_DONE (-1)   → [0x00]  (single byte sentinel)
//   positive ndx    → diff = ndx − prev_positive
//                     if 1 ≤ diff ≤ 0xFD → [diff]
//                     if diff == 0 or 0xFE ≤ diff ≤ 0x7FFF → [0xFE, hi, lo]
//                     else → [0xFE, hi|0x80, byte0, byte1, byte2]  (4-byte abs)
//   negative ndx    → [0xFF, <same encoding of -ndx against prev_negative>]
//
// prev_positive starts at -1, prev_negative starts at 1.

use std::cell::Cell;

// C rsync uses SEPARATE static prev_positive/prev_negative state inside
// read_ndx() and write_ndx() (each function has its own statics). Mirror that
// here with two independent pairs of thread-locals — sharing them would cause
// echo writes to encode as 0xFE-prefixed deltas and corrupt the stream.
thread_local! {
    static NDX_READ_PREV_POS: Cell<i32> = Cell::new(-1);
    static NDX_READ_PREV_NEG: Cell<i32> = Cell::new(1);
    static NDX_WRITE_PREV_POS: Cell<i32> = Cell::new(-1);
    static NDX_WRITE_PREV_NEG: Cell<i32> = Cell::new(1);
}

/// Reset the NDX delta state (call at the start of each connection).
pub fn reset_ndx_state() {
    NDX_READ_PREV_POS.with(|c| c.set(-1));
    NDX_READ_PREV_NEG.with(|c| c.set(1));
    NDX_WRITE_PREV_POS.with(|c| c.set(-1));
    NDX_WRITE_PREV_NEG.with(|c| c.set(1));
}

/// Read a delta-encoded file index (protocol 30+).
///
/// Returns the decoded NDX value, including NDX_DONE (-1).
pub fn read_ndx<R: Read>(r: &mut R) -> anyhow::Result<i32> {
    let b0 = read_byte(r)?;
    if b0 == 0x00 {
        return Ok(crate::protocol::constants::NDX_DONE);
    }

    let negative = b0 == 0xFF;
    let first = if negative { read_byte(r)? } else { b0 };

    // Decode the number (either absolute or as a delta from prev).
    let (is_absolute, raw): (bool, i32) = if first == 0xFE {
        let hi = read_byte(r)?;
        let lo = read_byte(r)?;
        if hi & 0x80 != 0 {
            // 4-byte absolute value encoded as:
            // byte0 = (ndx >> 24) | 0x80, byte1 = ndx & 0xFF,
            // byte2 = (ndx >> 8) & 0xFF, byte3 = (ndx >> 16) & 0xFF
            let b1 = read_byte(r)?;
            let b2 = read_byte(r)?;
            let n = (((hi & !0x80) as u32) << 24)
                | ((lo as u32))
                | ((b1 as u32) << 8)
                | ((b2 as u32) << 16);
            (true, n as i32)
        } else {
            // 2-byte delta
            let diff = ((hi as i32) << 8) | (lo as i32);
            (false, diff)
        }
    } else {
        // 1-byte delta
        (false, first as i32)
    };

    let num = if is_absolute {
        if negative {
            NDX_READ_PREV_NEG.with(|c| c.set(raw));
        } else {
            NDX_READ_PREV_POS.with(|c| c.set(raw));
        }
        raw
    } else {
        if negative {
            NDX_READ_PREV_NEG.with(|c| {
                let v = c.get() + raw;
                c.set(v);
                v
            })
        } else {
            NDX_READ_PREV_POS.with(|c| {
                let v = c.get() + raw;
                c.set(v);
                v
            })
        }
    };

    Ok(if negative { -num } else { num })
}

/// Write a delta-encoded file index (protocol 30+).
pub fn write_ndx<W: Write>(w: &mut W, ndx: i32) -> anyhow::Result<()> {
    if ndx == crate::protocol::constants::NDX_DONE {
        w.write_all(&[0x00])?;
        return Ok(());
    }

    let mut b = [0u8; 6];
    let mut cnt = 0usize;

    let (diff, is_neg, abs_ndx) = if ndx >= 0 {
        let prev = NDX_WRITE_PREV_POS.with(|c| c.get());
        let d = ndx - prev;
        NDX_WRITE_PREV_POS.with(|c| c.set(ndx));
        (d, false, ndx)
    } else {
        b[cnt] = 0xFF;
        cnt += 1;
        let pos = -ndx;
        let prev = NDX_WRITE_PREV_NEG.with(|c| c.get());
        let d = pos - prev;
        NDX_WRITE_PREV_NEG.with(|c| c.set(pos));
        (d, true, pos)
    };
    let _ = is_neg;

    if diff >= 1 && diff < 0xFE {
        b[cnt] = diff as u8;
        cnt += 1;
    } else if diff < 0 || diff > 0x7FFF {
        b[cnt] = 0xFE;
        cnt += 1;
        b[cnt] = ((abs_ndx >> 24) as u8) | 0x80;
        cnt += 1;
        b[cnt] = abs_ndx as u8;
        cnt += 1;
        b[cnt] = (abs_ndx >> 8) as u8;
        cnt += 1;
        b[cnt] = (abs_ndx >> 16) as u8;
        cnt += 1;
    } else {
        b[cnt] = 0xFE;
        cnt += 1;
        b[cnt] = (diff >> 8) as u8;
        cnt += 1;
        b[cnt] = diff as u8;
        cnt += 1;
    }
    w.write_all(&b[..cnt])?;
    Ok(())
}


//
// Wire format (matches C rsync's write_vstring / read_vstring in io.c):
//   len <= 0x7F:  1-byte length header + string bytes
//   len <= 0x7FFF: 2-byte length header (high byte = (len >> 8) | 0x80, low byte = len & 0xFF)
//                  + string bytes

pub fn read_vstring<R: Read>(r: &mut R) -> anyhow::Result<String> {
    let b0 = read_byte(r)?;
    let len: usize = if b0 & 0x80 != 0 {
        let b1 = read_byte(r)?;
        ((b0 as usize & 0x7F) << 8) | b1 as usize
    } else {
        b0 as usize
    };
    let mut buf = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut buf)?;
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn write_vstring<W: Write>(w: &mut W, s: &str) -> anyhow::Result<()> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len > 0x7FFF {
        anyhow::bail!("vstring too long: {}", len);
    }
    if len > 0x7F {
        w.write_all(&[((len >> 8) as u8) | 0x80, (len & 0xFF) as u8])?;
    } else {
        w.write_all(&[len as u8])?;
    }
    if len > 0 {
        w.write_all(bytes)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn encode_varint(x: i32) -> Vec<u8> {
        let mut buf = Vec::new();
        write_varint(&mut buf, x).unwrap();
        buf
    }

    fn decode_varint(bytes: &[u8]) -> i32 {
        read_varint(&mut Cursor::new(bytes)).unwrap()
    }

    fn encode_varlong(x: i64, mb: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        write_varlong(&mut buf, x, mb).unwrap();
        buf
    }

    fn decode_varlong(bytes: &[u8], mb: u8) -> i64 {
        read_varlong(&mut Cursor::new(bytes), mb).unwrap()
    }

    // ── varint round-trips ────────────────────────────────────────────────

    #[test]
    fn varint_zero() {
        assert_eq!(encode_varint(0), vec![0x00]);
        assert_eq!(decode_varint(&[0x00]), 0);
    }

    #[test]
    fn varint_one() {
        assert_eq!(encode_varint(1), vec![0x01]);
        assert_eq!(decode_varint(&[0x01]), 1);
    }

    #[test]
    fn varint_127() {
        assert_eq!(encode_varint(127), vec![0x7F]);
        assert_eq!(decode_varint(&[0x7F]), 127);
    }

    #[test]
    fn varint_128() {
        // 128 needs 2 bytes: marker 0x80 (1 extra byte), data 0x80
        assert_eq!(encode_varint(128), vec![0x80, 0x80]);
        assert_eq!(decode_varint(&[0x80, 0x80]), 128);
    }

    #[test]
    fn varint_16383() {
        // 0x3FFF = 16383: fits in 14 bits → 2 bytes [0xBF, 0xFF]
        assert_eq!(encode_varint(16383), vec![0xBF, 0xFF]);
        assert_eq!(decode_varint(&[0xBF, 0xFF]), 16383);
    }

    #[test]
    fn varint_16384() {
        // 0x4000 = 16384: needs 2-byte data + 1-byte overhead → 3 bytes [0xC0, 0x00, 0x40]
        assert_eq!(encode_varint(16384), vec![0xC0, 0x00, 0x40]);
        assert_eq!(decode_varint(&[0xC0, 0x00, 0x40]), 16384);
    }

    #[test]
    fn varint_2097151() {
        // 0x1FFFFF: 3 bytes [0xDF, 0xFF, 0xFF]
        assert_eq!(encode_varint(0x1FFFFF), vec![0xDF, 0xFF, 0xFF]);
        assert_eq!(decode_varint(&[0xDF, 0xFF, 0xFF]), 0x1FFFFF);
    }

    #[test]
    fn varint_2097152() {
        // 0x200000: 4 bytes [0xE0, 0x00, 0x00, 0x20]
        assert_eq!(encode_varint(0x200000), vec![0xE0, 0x00, 0x00, 0x20]);
        assert_eq!(decode_varint(&[0xE0, 0x00, 0x00, 0x20]), 0x200000);
    }

    #[test]
    fn varint_max_positive() {
        // i32::MAX = 0x7FFFFFFF: 5 bytes
        let bytes = encode_varint(i32::MAX);
        assert_eq!(decode_varint(&bytes), i32::MAX);
    }

    #[test]
    fn varint_minus_one() {
        // -1 = 0xFFFFFFFF: all 4 bytes non-zero, needs 5 bytes [0xF0, 0xFF, 0xFF, 0xFF, 0xFF]
        assert_eq!(encode_varint(-1), vec![0xF0, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(decode_varint(&[0xF0, 0xFF, 0xFF, 0xFF, 0xFF]), -1);
    }

    #[test]
    fn varint_roundtrip_range() {
        for x in [
            0, 1, 63, 64, 127, 128, 255, 256, 16383, 16384, 1 << 20, 1 << 28,
            i32::MAX, -1, -128, i32::MIN,
        ] {
            let encoded = encode_varint(x);
            assert_eq!(decode_varint(&encoded), x, "roundtrip failed for {}", x);
        }
    }

    // ── varlong round-trips ───────────────────────────────────────────────

    #[test]
    fn varlong_zero_mb3() {
        let enc = encode_varlong(0, 3);
        assert_eq!(decode_varlong(&enc, 3), 0);
    }

    #[test]
    fn varlong_zero_mb4() {
        let enc = encode_varlong(0, 4);
        assert_eq!(decode_varlong(&enc, 4), 0);
    }

    #[test]
    fn varlong_one_mb3() {
        let enc = encode_varlong(1, 3);
        assert_eq!(decode_varlong(&enc, 3), 1);
    }

    #[test]
    fn varlong_file_length_mb3() {
        // A typical file length that fits in 3 bytes
        for x in [0i64, 1, 100, 0xFFFF, 0xFFFFFF, 0x1_0000_0000, i64::MAX / 2] {
            let enc = encode_varlong(x, 3);
            assert_eq!(decode_varlong(&enc, 3), x, "varlong({}, 3) roundtrip", x);
        }
    }

    #[test]
    fn varlong_timestamp_mb4() {
        for x in [0i64, 1, 1_700_000_000, i32::MAX as i64, i64::MAX / 4] {
            let enc = encode_varlong(x, 4);
            assert_eq!(decode_varlong(&enc, 4), x, "varlong({}, 4) roundtrip", x);
        }
    }

    // ── simple integer primitives ─────────────────────────────────────────

    #[test]
    fn read_write_int_roundtrip() {
        for x in [0i32, 1, -1, i32::MAX, i32::MIN, 0x12345678u32 as i32] {
            let mut buf = Vec::new();
            write_int(&mut buf, x).unwrap();
            assert_eq!(buf.len(), 4);
            let got = read_int(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(got, x);
        }
    }

    #[test]
    fn read_write_longint_roundtrip() {
        for x in [0i64, 1, i32::MAX as i64, i32::MAX as i64 + 1, -1, i64::MIN] {
            let mut buf = Vec::new();
            write_longint(&mut buf, x).unwrap();
            let got = read_longint(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(got, x, "longint({}) roundtrip", x);
        }
    }

    #[test]
    fn longint_small_is_4_bytes() {
        let mut buf = Vec::new();
        write_longint(&mut buf, 42).unwrap();
        assert_eq!(buf.len(), 4);
    }

    #[test]
    fn longint_large_is_12_bytes() {
        let mut buf = Vec::new();
        write_longint(&mut buf, i32::MAX as i64 + 1).unwrap();
        assert_eq!(buf.len(), 12);
        assert_eq!(&buf[..4], &[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn read_write_shortint_roundtrip() {
        for x in [0u16, 1, 0xFF, 0x100, u16::MAX] {
            let mut buf = Vec::new();
            write_shortint(&mut buf, x).unwrap();
            assert_eq!(buf.len(), 2);
            let got = read_shortint(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(got, x);
        }
    }

    #[test]
    fn read_write_byte_roundtrip() {
        for x in [0u8, 1, 127, 128, 255] {
            let mut buf = Vec::new();
            write_byte(&mut buf, x).unwrap();
            assert_eq!(buf.len(), 1);
            assert_eq!(read_byte(&mut Cursor::new(&buf)).unwrap(), x);
        }
    }
}
