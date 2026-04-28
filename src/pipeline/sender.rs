//! Sender pipeline stage — Rust port of `sender.c`.
//!
//! The sender loops over file indices supplied by the generator, reads the
//! receiver's block checksums (`SumHead` + `SumBuf`s), computes a rolling-
//! checksum delta against the local copy of each file, and writes the
//! resulting token stream (literals + block references) to the writer.

#![allow(dead_code)]

use std::io::{Read, Write};

use anyhow::Result;

use crate::checksum::rolling::checksum1;
use crate::checksum::strong::StrongChecksum;
use crate::delta::match_blocks::{
    read_sum_bufs, read_sum_head, write_sum_head, BlockHashTable, DeltaOp,
};
use crate::delta::token::TokenWriter;
use crate::fileops::slurp_file;
use crate::io::varint::{
    read_byte, read_int, read_ndx, read_shortint, read_vstring, write_byte, write_int, write_ndx,
    write_shortint, write_vstring,
};
use crate::protocol::constants::{
    CsumType, ITEM_BASIS_TYPE_FOLLOWS, ITEM_TRANSFER, ITEM_XNAME_FOLLOWS, NDX_DONE,
};
use crate::protocol::types::{FileList, Stats, SumBuf, SumHead};
use crate::util::{block_len_for_file, remainder_for_file, sum_count_for_file};

// ── Public struct ─────────────────────────────────────────────────────────────

/// Sender pipeline — mirrors `send_files()` in `sender.c`.
pub struct Sender<R: Read, W: Write> {
    reader: R,
    writer: W,
    pub stats: Stats,
    pub use_zlib: bool,
}

impl<R: Read, W: Write> Sender<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Sender { reader, writer, stats: Stats::default(), use_zlib: false }
    }

    pub fn with_compression(mut self, use_zlib: bool) -> Self {
        self.use_zlib = use_zlib;
        self
    }

    /// Run the sender loop.
    ///
    /// Generator (client-side) sends per file:
    ///   1. NDX (delta-encoded) — file index, or NDX_DONE to end a phase
    ///   2. iflags (shortint, 2 bytes LE) — item flags
    ///   3. fnamecmp_type (byte) — if ITEM_BASIS_TYPE_FOLLOWS
    ///   4. xname (vstring) — if ITEM_XNAME_FOLLOWS
    ///   5. SumHead + SumBufs — if ITEM_TRANSFER
    ///
    /// Sender (us) responds per file:
    ///   1. NDX + iflags + optional fields (echo back)
    ///   2. SumHead (our local file's block layout)
    ///   3. Data tokens (match_sums output)
    ///
    /// Phase protocol (protocol >= 29): 2 phases, each terminated by NDX_DONE.
    /// Between phases the sender echoes NDX_DONE.
    pub fn run(
        &mut self,
        flist: &FileList,
        base_dir: &std::path::Path,
        _csum_type: CsumType,
        protocol: u32,
        checksum_seed: i32,
    ) -> Result<()> {
        let max_phase = if protocol >= 29 { 2 } else { 1 };
        let mut phase = 0;

        loop {
            crate::rdebug!("[sender] waiting for NDX (phase={})...", phase);
            let idx = if protocol >= 30 {
                read_ndx(&mut self.reader)?
            } else {
                read_int(&mut self.reader)?
            };
            crate::rdebug!("[sender] got NDX={}", idx);

            if idx == NDX_DONE {
                phase += 1;
                crate::rdebug!("[sender] NDX_DONE, now phase={}, max_phase={}", phase, max_phase);
                if phase > max_phase {
                    break;
                }
                // Echo NDX_DONE back to receiver to signal end of our phase.
                if protocol >= 30 {
                    write_ndx(&mut self.writer, NDX_DONE)?;
                } else {
                    write_int(&mut self.writer, NDX_DONE)?;
                }
                self.writer.flush().ok();
                continue;
            }

            // Read iflags (protocol >= 29).
            let iflags: u32 = if protocol >= 29 {
                let f = read_shortint(&mut self.reader)? as u32;
                crate::rdebug!("[sender] iflags=0x{:04x}", f);
                f
            } else {
                ITEM_TRANSFER
            };

            // Read optional fnamecmp_type.
            let fnamecmp_type = if iflags & ITEM_BASIS_TYPE_FOLLOWS != 0 {
                read_byte(&mut self.reader)?
            } else {
                0
            };

            // Read optional xname.
            let xname = if iflags & ITEM_XNAME_FOLLOWS != 0 {
                read_vstring(&mut self.reader).unwrap_or_default()
            } else {
                String::new()
            };

            // Echo NDX + iflags back to receiver before sending data.
            if protocol >= 30 {
                write_ndx(&mut self.writer, idx)?;
            } else {
                write_int(&mut self.writer, idx)?;
            }
            if protocol >= 29 {
                write_shortint(&mut self.writer, iflags as u16)?;
            }
            // Echo optional fields that receiver must also see.
            if iflags & ITEM_BASIS_TYPE_FOLLOWS != 0 {
                write_byte(&mut self.writer, fnamecmp_type)?;
            }
            if iflags & ITEM_XNAME_FOLLOWS != 0 {
                write_vstring(&mut self.writer, &xname)?;
            }

            if iflags & ITEM_TRANSFER == 0 {
                // No data transfer needed for this file (e.g. attrs-only update).
                self.writer.flush().ok();
                continue;
            }

            // Read the receiver's block checksums for this file.
            let head = read_sum_head(&mut self.reader)?;
            crate::rdebug!("[sender] sum_head: count={} blen={} s2len={} rem={}",
                head.count, head.blength, head.s2length, head.remainder);
            let sums = read_sum_bufs(&mut self.reader, &head)?;

            // Helper: when we cannot send file data, still emit the receiver's
            // expected wire frame (sum_head + EOF token + file_sum of zeros) so
            // the protocol stream stays aligned. Mirrors C's behaviour when a
            // sender-side error occurs after match_sums has been entered.

            // Look up the file entry.
            let fi = match flist.get_by_ndx(idx) {
                Some(f) => f,
                None => {
                    crate::rdebug!("[sender] unknown ndx {} -> skipped", idx);
                    log::warn!("sender: unknown file index {}", idx);
                    send_skipped_xfer(&mut self.writer, &head)?;
                    continue;
                }
            };

            crate::rdebug!("[sender] file ndx={} path={:?} mode=0o{:o} regular={}",
                idx, fi.path(), fi.mode, fi.is_regular());

            if !fi.is_regular() {
                crate::rdebug!("[sender] not regular -> skipped");
                send_skipped_xfer(&mut self.writer, &head)?;
                continue;
            }

            let file_path = base_dir.join(fi.path());
            crate::rdebug!("[sender] reading {:?}", file_path);

            let data = match slurp_file(&file_path) {
                Ok(d) => d,
                Err(e) => {
                    crate::rdebug!("[sender] slurp failed: {}", e);
                    log::warn!("sender: cannot read {:?}: {}", file_path, e);
                    send_skipped_xfer(&mut self.writer, &head)?;
                    continue;
                }
            };

            let file_len = data.len();

            // Build the hash table and compute delta operations.
            let table = BlockHashTable::build(&head, &sums);
            let strong_len = head.s2length as usize;
            let ops = crate::delta::match_blocks::find_matches_seeded(
                &data,
                &table,
                strong_len,
                checksum_seed as u32,
            );

            // Echo sum_head to receiver (C: write_sum_head before match_sums).
            write_sum_head(&mut self.writer, &head)?;

            // Write token stream (borrow of writer released after finish()).
            {
                let blen = head.blength as usize;
                if self.use_zlib {
                    let mut tw = crate::delta::DeflatedTokenWriter::new(&mut self.writer);
                    for op in &ops {
                        match op {
                            DeltaOp::Literal { offset, len } => {
                                let off = *offset as usize;
                                let l = *len as usize;
                                tw.send_literal(&data[off..off + l])?;
                            }
                            DeltaOp::Match { block_idx, offset } => {
                                let off = *offset as usize;
                                let end = (off + blen).min(data.len());
                                tw.send_block_match(*block_idx as i32, &data[off..end])?;
                            }
                        }
                    }
                    tw.finish()?;
                } else {
                    let mut tw = TokenWriter::new(&mut self.writer);
                    for op in &ops {
                        match op {
                            DeltaOp::Literal { offset, len } => {
                                let off = *offset as usize;
                                let l = *len as usize;
                                tw.send_literal(&data[off..off + l])?;
                            }
                            DeltaOp::Match { block_idx, .. } => {
                                tw.send_block_match(*block_idx as i32)?;
                            }
                        }
                    }
                    tw.finish()?;
                }
            }

            // Write the whole-file transfer checksum (xfer_sum). For protocol >= 30
            // the negotiated checksum is plain MD5(data) — sum_init() in C
            // checksum.c does NOT feed checksum_seed for CSUM_MD5 (only the old
            // MD4 variants seed). Per-block s2 sums use the seed; this one does not.
            let file_sum = StrongChecksum::compute(
                &data,
                crate::checksum::strong::ChecksumType::Md5,
                0,
                false,
            );
            let _ = checksum_seed;
            crate::rdebug!("[sender] writing file_sum {} bytes ({:02x?})", &file_sum[..16].len(), &file_sum[..16]);
            self.writer.write_all(&file_sum[..16])?;

            self.writer.flush().ok();

            // Update statistics.
            self.stats.total_written += file_len as i64;
            self.stats.xferred_files += 1;
        }

        // Final goodbye NDX_DONE — mirrors `write_ndx(f_out, NDX_DONE)` at
        // the bottom of C's send_files() (sender.c:464).
        if protocol >= 30 {
            write_ndx(&mut self.writer, NDX_DONE)?;
        } else {
            write_int(&mut self.writer, NDX_DONE)?;
        }
        self.writer.flush().ok();

        Ok(())
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Send a bare EOF token (empty file transfer).
fn send_empty_eof<W: Write>(w: &mut W) -> Result<()> {
    write_int(w, 0)
}

/// Emit the wire frame for a "skipped" transfer so the receiver stays in sync:
/// sum_head + EOF token + zero-filled file checksum (md5 = 16 bytes).
fn send_skipped_xfer<W: Write>(w: &mut W, head: &SumHead) -> Result<()> {
    write_sum_head(w, head)?;
    write_int(w, 0)?;
    w.write_all(&[0u8; 16])?;
    Ok(())
}

/// Build a `SumHead` for a local file of `file_len` bytes.
/// Used when the sender needs to describe its own file layout.
fn make_sum_head(file_len: u64, sum_len: i32) -> SumHead {
    let size = file_len as i64;
    let blength = block_len_for_file(size);
    let count = sum_count_for_file(size, blength);
    let remainder = remainder_for_file(size, blength);
    SumHead { count, blength, s2length: sum_len, remainder }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compute and write block checksums for `data` to `w`.
///
/// Called by the generator when it needs to describe an existing local file to
/// the sender so the sender can compute a minimal delta.
///
/// Wire format per block:
///   `write_int(rolling_checksum)` followed by `sum2[..s2length]`.
pub fn generate_and_write_checksums<W: Write>(
    w: &mut W,
    data: &[u8],
    head: &SumHead,
    csum_type: CsumType,
    seed: u32,
    proper_seed_order: bool,
) -> Result<()> {
    if head.count == 0 || head.blength == 0 {
        return Ok(());
    }

    let blength = head.blength as usize;
    let s2length = head.s2length as usize;
    let count = head.count as usize;
    let strong_type = super::csum_type_to_checksum_type(csum_type);

    for i in 0..count {
        let is_last = i == count - 1;
        let block_len = if is_last && head.remainder != 0 {
            head.remainder as usize
        } else {
            blength
        };
        let offset = i * blength;
        let end = (offset + block_len).min(data.len());
        let block = &data[offset..end];

        let sum1 = checksum1(block);
        let sum2 = StrongChecksum::compute(block, strong_type, seed, proper_seed_order);

        write_int(w, sum1 as i32)?;
        let copy_len = s2length.min(sum2.len());
        w.write_all(&sum2[..copy_len])?;
    }

    Ok(())
}

/// Compute in-memory block-checksum entries for `data`.
///
/// Returns a `Vec<SumBuf>` suitable for building a `BlockHashTable` or
/// writing with `write_sum_bufs`.
pub(crate) fn compute_sum_bufs(
    data: &[u8],
    head: &SumHead,
    csum_type: CsumType,
    seed: u32,
    proper_seed_order: bool,
) -> Vec<SumBuf> {
    if head.count == 0 || head.blength == 0 {
        return Vec::new();
    }

    let blength = head.blength as usize;
    let s2length = head.s2length as usize;
    let count = head.count as usize;
    let strong_type = super::csum_type_to_checksum_type(csum_type);
    let mut sums = Vec::with_capacity(count);

    for i in 0..count {
        let is_last = i == count - 1;
        let block_len = if is_last && head.remainder != 0 {
            head.remainder as usize
        } else {
            blength
        };
        let offset = i * blength;
        let end = (offset + block_len).min(data.len());
        let block = &data[offset..end];

        let sum1 = checksum1(block);
        let sum2_full = StrongChecksum::compute(block, strong_type, seed, proper_seed_order);
        let sum2 = sum2_full[..s2length.min(sum2_full.len())].to_vec();

        sums.push(SumBuf {
            offset: offset as i64,
            len: block_len as i32,
            sum1,
            chain: -1,
            flags: 0,
            sum2,
        });
    }

    sums
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── make_sum_head ─────────────────────────────────────────────────────────

    #[test]
    fn make_sum_head_exact_multiple() {
        // 1400 bytes with blength=700 → count=2, remainder=0.
        let head = make_sum_head(1400, 16);
        assert_eq!(head.blength, 700);
        assert_eq!(head.count, 2);
        assert_eq!(head.remainder, 0);
        assert_eq!(head.s2length, 16);
    }

    #[test]
    fn make_sum_head_with_remainder() {
        // 1000 bytes with blength=700 → count=2, remainder=300.
        let head = make_sum_head(1000, 16);
        assert_eq!(head.blength, 700);
        assert_eq!(head.count, 2);
        assert_eq!(head.remainder, 300);
        assert_eq!(head.s2length, 16);
    }

    #[test]
    fn make_sum_head_empty_file() {
        let head = make_sum_head(0, 16);
        assert_eq!(head.count, 0);
    }

    // ── generate_and_write_checksums ──────────────────────────────────────────

    #[test]
    fn generate_checksums_round_trip() {
        // Build checksums for a known block; read them back and verify.
        let data = b"hello world 12345678"; // 20 bytes
        let blength = 10i32;
        let count = 2i32;
        let remainder = 0i32;
        let s2length = 16i32;
        let head = SumHead { count, blength, s2length, remainder };

        let mut buf = Vec::new();
        generate_and_write_checksums(&mut buf, data, &head, CsumType::Md5, 0, false).unwrap();

        // Read back: count × (4-byte sum1 + 16-byte sum2)
        assert_eq!(buf.len(), count as usize * (4 + s2length as usize));

        let sums = crate::delta::match_blocks::read_sum_bufs(
            &mut Cursor::new(&buf),
            &head,
        )
        .unwrap();
        assert_eq!(sums.len(), 2);
        assert_eq!(sums[0].sum1, checksum1(&data[..10]));
        assert_eq!(sums[1].sum1, checksum1(&data[10..20]));
    }

    // ── Sender integration ────────────────────────────────────────────────────

    #[test]
    fn sender_ndx_done_immediately() {
        // A sender that receives NDX_DONE right away should return Ok without
        // writing anything.
        use crate::io::varint::write_ndx;
        use crate::protocol::types::FileList;

        // Build input: NDX_DONE three times (phase 0 → 1 → 2 → 3 break).
        let mut input = Vec::new();
        for _ in 0..3 {
            write_ndx(&mut input, NDX_DONE).unwrap();
        }
        // Reset the global write-side prev state used while filling the buffer.
        crate::io::varint::reset_ndx_state();

        let output = Vec::new();
        let flist = FileList::new();

        let mut sender = Sender::new(Cursor::new(input), output);
        sender.run(&flist, std::path::Path::new("."), CsumType::Md5, 31, 0).unwrap();
        assert_eq!(sender.stats.xferred_files, 0);
    }
}
