//! Block-matching algorithm — Rust port of match.c / generator checksum I/O.
//!
//! SumHead wire format (write_sum_head in io.c, protocol >= 27):
//!   count     i32
//!   blength   i32
//!   s2length  i32
//!   remainder i32
//!
//! SumBuf wire format (receive_sums in sender.c):
//!   sum1  i32 (4 bytes LE, reinterpreted as u32)
//!   sum2  [u8; s2length]

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};

use anyhow::Result;
use md5::{Digest, Md5};

use crate::checksum::rolling::RollingChecksum;
use crate::io::varint::{read_int, write_int};
use crate::protocol::types::{SumBuf, SumHead};

// ── Wire I/O ──────────────────────────────────────────────────────────────────

/// Read a `SumHead` from the wire (protocol >= 27 format).
pub fn read_sum_head<R: Read>(r: &mut R) -> Result<SumHead> {
    let count = read_int(r)?;
    let blength = read_int(r)?;
    let s2length = read_int(r)?;
    let remainder = read_int(r)?;
    Ok(SumHead { count, blength, s2length, remainder })
}

/// Write a `SumHead` to the wire (protocol >= 27 format).
pub fn write_sum_head<W: Write>(w: &mut W, head: &SumHead) -> Result<()> {
    write_int(w, head.count)?;
    write_int(w, head.blength)?;
    write_int(w, head.s2length)?;
    write_int(w, head.remainder)?;
    Ok(())
}

/// Read all block-checksum entries described by `head`.
pub fn read_sum_bufs<R: Read>(r: &mut R, head: &SumHead) -> Result<Vec<SumBuf>> {
    let count = head.count as usize;
    let s2len = head.s2length as usize;
    let blength = head.blength;
    let remainder = head.remainder;

    let mut sums = Vec::with_capacity(count);
    let mut offset = 0i64;

    for i in 0..count {
        // sum1 is stored as a raw i32 on the wire (signed integer encoding),
        // but logically it is an unsigned 32-bit rolling checksum.
        let sum1 = read_int(r)? as u32;
        let mut sum2 = vec![0u8; s2len];
        r.read_exact(&mut sum2)?;

        let len = if i == count - 1 && remainder != 0 { remainder } else { blength };

        sums.push(SumBuf {
            offset,
            len,
            sum1,
            chain: -1,
            flags: 0,
            sum2,
        });
        offset += len as i64;
    }
    Ok(sums)
}

/// Write block-checksum entries to the wire.
pub fn write_sum_bufs<W: Write>(w: &mut W, sums: &[SumBuf]) -> Result<()> {
    for s in sums {
        write_int(w, s.sum1 as i32)?;
        w.write_all(&s.sum2)?;
    }
    Ok(())
}

// ── Hash table ────────────────────────────────────────────────────────────────

/// In-memory lookup structure built from the remote's block checksums.
pub struct BlockHashTable {
    /// rolling checksum → list of (block index, strong-checksum bytes)
    table: HashMap<u32, Vec<(usize, Vec<u8>)>>,
    pub head: SumHead,
    pub sums: Vec<SumBuf>,
}

impl BlockHashTable {
    pub fn build(head: &SumHead, sums: &[SumBuf]) -> Self {
        let mut table: HashMap<u32, Vec<(usize, Vec<u8>)>> =
            HashMap::with_capacity(sums.len());

        for (idx, s) in sums.iter().enumerate() {
            table.entry(s.sum1).or_default().push((idx, s.sum2.clone()));
        }

        Self { table, head: *head, sums: sums.to_vec() }
    }

    /// Return all candidates whose rolling checksum matches `sum1`.
    pub fn find_rolling(&self, sum1: u32) -> Option<&Vec<(usize, Vec<u8>)>> {
        self.table.get(&sum1)
    }
}

// ── Delta operation ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum DeltaOp {
    Literal { offset: u64, len: u32 },
    Match { block_idx: usize, offset: u64 },
}

// ── Core matching algorithm ───────────────────────────────────────────────────

/// Find matching blocks between `data` (sender) and `table` (receiver's blocks).
///
/// Uses a sliding rolling-checksum window of `head.blength` bytes.
/// On a rolling match, verifies the strong (MD5) checksum before confirming.
pub fn find_matches(data: &[u8], table: &BlockHashTable, strong_len: usize) -> Vec<DeltaOp> {
    find_matches_seeded(data, table, strong_len, 0)
}

/// Like `find_matches` but uses `seed` when computing the per-window strong
/// checksum (C `get_checksum2` appends seed bytes for protocol-negotiated MD5
/// blocks).
pub fn find_matches_seeded(
    data: &[u8],
    table: &BlockHashTable,
    strong_len: usize,
    seed: u32,
) -> Vec<DeltaOp> {
    let mut ops = Vec::new();
    let blength = table.head.blength as usize;

    if blength == 0 || data.is_empty() {
        if !data.is_empty() {
            ops.push(DeltaOp::Literal { offset: 0, len: data.len() as u32 });
        }
        return ops;
    }

    let len = data.len();
    // last_match tracks the byte offset we have already covered (literal or matched).
    let mut last_match: usize = 0;

    if len < blength {
        // Entire data fits in less than one block — just emit as literal.
        ops.push(DeltaOp::Literal { offset: 0, len: len as u32 });
        return ops;
    }

    // Initialise the rolling checksum on the first window.
    let mut rc = RollingChecksum::new();
    rc.init(&data[0..blength]);

    let end = len - blength; // last valid window start (inclusive)
    let mut offset: usize = 0;

    loop {
        let sum = rc.value();

        if let Some(candidates) = table.find_rolling(sum) {
            // Verify block length matches (last block may be shorter).
            let window = &data[offset..offset + blength];

            // Compute strong checksum once per position (lazy, only when needed).
            let strong: Option<Vec<u8>> = if strong_len > 0 {
                let mut h = Md5::new();
                h.update(window);
                if seed != 0 {
                    h.update(seed.to_le_bytes());
                }
                let digest = h.finalize();
                Some(digest[..strong_len.min(digest.len())].to_vec())
            } else {
                None
            };

            let matched_idx = candidates.iter().find_map(|(idx, stored_sum2)| {
                // Block lengths must agree — compare against this entry's len.
                let entry_len = table.sums[*idx].len as usize;
                if entry_len != blength {
                    return None;
                }
                // Rolling checksum already matched; now verify strong checksum.
                match &strong {
                    Some(s) => {
                        let cmp_len = strong_len.min(stored_sum2.len());
                        if s[..cmp_len] == stored_sum2[..cmp_len] {
                            Some(*idx)
                        } else {
                            None
                        }
                    }
                    None => Some(*idx), // no strong check requested
                }
            });

            if let Some(block_idx) = matched_idx {
                // Emit any un-matched literal bytes before this match.
                if offset > last_match {
                    ops.push(DeltaOp::Literal {
                        offset: last_match as u64,
                        len: (offset - last_match) as u32,
                    });
                }
                ops.push(DeltaOp::Match { block_idx, offset: offset as u64 });
                last_match = offset + blength;
                offset = last_match;

                // Re-initialise rolling checksum for the next window.
                if offset + blength <= len {
                    rc.init(&data[offset..offset + blength]);
                } else {
                    break;
                }
                continue;
            }
        }

        if offset >= end {
            break;
        }

        // Slide the window one byte to the right.
        let old = data[offset];
        let new = data[offset + blength];
        rc.roll(old, new);
        offset += 1;
    }

    // Any remaining bytes after the last match become a literal.
    if last_match < len {
        ops.push(DeltaOp::Literal {
            offset: last_match as u64,
            len: (len - last_match) as u32,
        });
    }

    ops
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum::rolling::checksum1;
    use std::io::Cursor;

    fn make_head(count: i32, blength: i32, remainder: i32, s2length: i32) -> SumHead {
        SumHead { count, blength, remainder, s2length }
    }

    fn md5_of(data: &[u8]) -> Vec<u8> {
        let mut h = Md5::new();
        h.update(data);
        h.finalize().to_vec()
    }

    #[test]
    fn sum_head_round_trip() {
        let head = make_head(10, 700, 300, 16);
        let mut buf = Vec::new();
        write_sum_head(&mut buf, &head).unwrap();
        assert_eq!(buf.len(), 16); // 4 × i32
        let got = read_sum_head(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(got.count, head.count);
        assert_eq!(got.blength, head.blength);
        assert_eq!(got.remainder, head.remainder);
        assert_eq!(got.s2length, head.s2length);
    }

    #[test]
    fn sum_bufs_round_trip() {
        let head = make_head(2, 4, 3, 16);
        let _data = b"hello world";
        let sums = vec![
            SumBuf {
                offset: 0,
                len: 4,
                sum1: checksum1(b"hell"),
                chain: -1,
                flags: 0,
                sum2: md5_of(b"hell"),
            },
            SumBuf {
                offset: 4,
                len: 4,
                sum1: checksum1(b"o wo"),
                chain: -1,
                flags: 0,
                sum2: md5_of(b"o wo"),
            },
        ];

        let mut buf = Vec::new();
        write_sum_bufs(&mut buf, &sums).unwrap();

        let got = read_sum_bufs(&mut Cursor::new(&buf), &head).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].sum1, sums[0].sum1);
        assert_eq!(got[0].sum2, sums[0].sum2);
        assert_eq!(got[1].sum1, sums[1].sum1);
    }

    #[test]
    fn find_matches_identical_data() {
        // Remote and local data are identical — every block should match.
        let data = b"abcdabcdabcd"; // 12 bytes
        let blength = 4i32;
        let s2len = 16usize;

        let count = data.len() / blength as usize;
        let head = make_head(count as i32, blength, 0, s2len as i32);

        let sums: Vec<SumBuf> = (0..count)
            .map(|i| {
                let block = &data[i * blength as usize..(i + 1) * blength as usize];
                SumBuf {
                    offset: (i * blength as usize) as i64,
                    len: blength,
                    sum1: checksum1(block),
                    chain: -1,
                    flags: 0,
                    sum2: md5_of(block),
                }
            })
            .collect();

        let table = BlockHashTable::build(&head, &sums);
        let ops = find_matches(data, &table, s2len);

        let matches: Vec<_> = ops.iter().filter(|o| matches!(o, DeltaOp::Match { .. })).collect();
        assert_eq!(matches.len(), count, "all blocks should match; got {:?}", ops);
        let literals: Vec<_> = ops.iter().filter(|o| matches!(o, DeltaOp::Literal { .. })).collect();
        assert!(literals.is_empty(), "no literals expected; got {:?}", ops);
    }

    #[test]
    fn find_matches_no_blocks() {
        // Empty remote checksums → all local data is literal.
        let data = b"hello world";
        let head = make_head(0, 700, 0, 16);
        let table = BlockHashTable::build(&head, &[]);
        let ops = find_matches(data, &table, 16);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], DeltaOp::Literal { offset: 0, len: 11 }));
    }

    #[test]
    fn find_matches_partial() {
        // "hello world" — first 5 bytes match, last 6 are literal.
        let data = b"hello world";
        let blength = 5i32;
        let block = &data[..blength as usize];
        let head = make_head(1, blength, 0, 16);
        let sums = vec![SumBuf {
            offset: 0,
            len: blength,
            sum1: checksum1(block),
            chain: -1,
            flags: 0,
            sum2: md5_of(block),
        }];
        let table = BlockHashTable::build(&head, &sums);
        let ops = find_matches(data, &table, 16);

        let has_match = ops.iter().any(|o| matches!(o, DeltaOp::Match { block_idx: 0, .. }));
        assert!(has_match, "expected a match; got {:?}", ops);
        let total_literal: u32 = ops
            .iter()
            .filter_map(|o| if let DeltaOp::Literal { len, .. } = o { Some(*len) } else { None })
            .sum();
        assert_eq!(total_literal, (data.len() - blength as usize) as u32);
    }
}
