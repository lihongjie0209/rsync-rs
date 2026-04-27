//! Compressed (deflated) token wire format from `token.c`'s
//! `send_deflated_token` / `recv_deflated_token` / `see_deflate_token`.
//!
//! Wire format byte flags (token.c:323-330):
//!   END_FLAG       = 0x00         — end of stream
//!   TOKEN_LONG     = 0x20         — followed by int32 absolute token
//!   TOKENRUN_LONG  = 0x21         — int32 abs token + 2-byte LE run count
//!   DEFLATED_DATA  = 0x40..=0x7F  — low 6 bits = high byte of length;
//!                                   next byte = low byte; then `length` bytes
//!                                   of raw deflate output (window=-15)
//!   TOKEN_REL      = 0x80..=0xBF  — low 6 bits = relative offset
//!   TOKENRUN_REL   = 0xC0..=0xFF  — relative + 2-byte LE run count
//!
//! "Relative offset" `r = run_start - last_run_end`.
//! "Run count" `n = last_token - run_start` (extras after the first token).
//!
//! Each literal flush ends with a Z_SYNC_FLUSH whose trailing 4 bytes
//! `00 00 ff ff` are TRIMMED from the wire and re-injected by the receiver
//! before the next non-DEFLATED_DATA byte.
//!
//! Block-match dictionary sync: when a block-match token is emitted, BOTH
//! sides must feed the matched block bytes through their respective
//! deflate/inflate streams. Sender uses `compress(Sync)` and discards
//! output. Receiver uses a fake stored-block header (`00 LL LL ~LL ~LL`)
//! followed by the bytes, then `decompress(Sync)`.

#![allow(dead_code)]

use std::io::{Read, Write};

use anyhow::{anyhow, Result};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::delta::token::Token;
use crate::io::varint::{read_int, write_int};

const END_FLAG: u8 = 0x00;
const TOKEN_LONG: u8 = 0x20;
const TOKENRUN_LONG: u8 = 0x21;
const DEFLATED_DATA: u8 = 0x40;
const TOKEN_REL: u8 = 0x80;
const TOKENRUN_REL: u8 = 0xC0;

const MAX_DATA_COUNT: usize = 16383;
const SYNC_TRAILER: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

// ─── Writer ───────────────────────────────────────────────────────────────────

pub struct DeflatedTokenWriter<W: Write> {
    inner: W,
    deflate: Compress,
    /// Pending literal bytes not yet sync-flushed.
    pending_in: Vec<u8>,
    /// True once we've sync-flushed at least once and emitted DEFLATED_DATA.
    have_unflushed_input: bool,
    /// State of the run encoder.
    last_token: i32,
    run_start: i32,
    last_run_end: i32,
    /// True when a token run has been started (via emit_token) but not yet
    /// closed (via close_run). Must be closed before emitting any
    /// DEFLATED_DATA frames or END_FLAG.
    run_open: bool,
}

impl<W: Write> DeflatedTokenWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            // Raw deflate with 15-bit window (matches deflateInit2 with -15).
            deflate: Compress::new(Compression::default(), false),
            pending_in: Vec::new(),
            have_unflushed_input: false,
            last_token: -1,
            run_start: 0,
            last_run_end: 0,
            run_open: false,
        }
    }

    /// Buffer literal bytes (will be emitted at the next sync-flush boundary).
    pub fn send_literal(&mut self, data: &[u8]) -> Result<()> {
        if !data.is_empty() {
            self.pending_in.extend_from_slice(data);
            self.have_unflushed_input = true;
        }
        Ok(())
    }

    /// Send a block-match reference (0-based index) and feed `block_bytes`
    /// (the matched bytes from the SOURCE file at the match offset) into
    /// the deflate stream's dictionary.
    pub fn send_block_match(&mut self, block_idx: i32, block_bytes: &[u8]) -> Result<()> {
        // Flush any pending literal first (DEFLATED_DATA frames + trim).
        self.flush_pending_literal()?;
        // Emit the run/token byte for this block index.
        self.emit_token(block_idx)?;
        // Feed matched bytes into deflate for dict sync; discard output.
        self.feed_dict_bytes(block_bytes)?;
        Ok(())
    }

    /// Emit the END_FLAG and any final pending literal.
    pub fn finish(mut self) -> Result<W> {
        if self.run_open {
            self.close_run()?;
        }
        self.flush_pending_literal()?;
        self.inner.write_all(&[END_FLAG])?;
        Ok(self.inner)
    }

    // ── private ────────────────────────────────────────────────────────────

    /// Run/token byte emission per token.c:385-401.
    /// We don't aggregate runs greedily here because each call to send_block_match
    /// is followed by either another match or a literal flush. To match the C
    /// behavior of run-aggregation across consecutive matches, callers should
    /// emit consecutive matches without intervening literals.
    fn emit_token(&mut self, token: i32) -> Result<()> {
        // Initial state.
        if self.last_token == -1 {
            self.run_start = token;
            self.last_token = token;
            self.run_open = true;
            return Ok(());
        }
        // Continue current run if consecutive and within run-length limit.
        if token == self.last_token + 1 && token < self.run_start + 65536 {
            self.last_token = token;
            return Ok(());
        }
        // Otherwise, close the current run and start a new one.
        self.close_run()?;
        self.run_start = token;
        self.last_token = token;
        self.run_open = true;
        Ok(())
    }

    fn close_run(&mut self) -> Result<()> {
        let r = self.run_start - self.last_run_end;
        let n = self.last_token - self.run_start; // extras after first
        if (0..=63).contains(&r) {
            let base = if n == 0 { TOKEN_REL } else { TOKENRUN_REL };
            self.inner.write_all(&[base + r as u8])?;
        } else {
            let base = if n == 0 { TOKEN_LONG } else { TOKENRUN_LONG };
            self.inner.write_all(&[base])?;
            write_int(&mut self.inner, self.run_start)?;
        }
        if n != 0 {
            self.inner.write_all(&[(n & 0xff) as u8, ((n >> 8) & 0xff) as u8])?;
        }
        self.last_run_end = self.last_token;
        self.run_open = false;
        Ok(())
    }

    /// Sync-flush pending literal bytes, trim trailing 00 00 ff ff once,
    /// and write DEFLATED_DATA frames of up to MAX_DATA_COUNT bytes each.
    fn flush_pending_literal(&mut self) -> Result<()> {
        if !self.have_unflushed_input {
            return Ok(());
        }
        // Close any open token run before emitting literal data on the wire.
        if self.run_open {
            self.close_run()?;
        }
        let mut out = Vec::with_capacity(self.pending_in.len() + 64);
        // Phase 1: drain all pending input through deflate(NoFlush).
        let mut in_pos = 0;
        while in_pos < self.pending_in.len() {
            let mut tmp = [0u8; 4096];
            let before_in = self.deflate.total_in();
            let before_out = self.deflate.total_out();
            let status = self
                .deflate
                .compress(&self.pending_in[in_pos..], &mut tmp, FlushCompress::None)
                .map_err(|e| anyhow!("deflate compress failed: {:?}", e))?;
            let consumed = (self.deflate.total_in() - before_in) as usize;
            let produced = (self.deflate.total_out() - before_out) as usize;
            in_pos += consumed;
            out.extend_from_slice(&tmp[..produced]);
            if matches!(status, Status::BufError) && consumed == 0 && produced == 0 {
                return Err(anyhow!("deflate stalled"));
            }
        }
        // Phase 2: sync-flush. Single call with large output buffer.
        let mut tmp = vec![0u8; out.len() + self.pending_in.len() + 256];
        let before_out = self.deflate.total_out();
        let _ = self
            .deflate
            .compress(&[], &mut tmp, FlushCompress::Sync)
            .map_err(|e| anyhow!("deflate sync flush failed: {:?}", e))?;
        let produced = (self.deflate.total_out() - before_out) as usize;
        out.extend_from_slice(&tmp[..produced]);
        // Trim trailing 00 00 ff ff (single trim per flush).
        if out.len() >= 4 && out[out.len() - 4..] == SYNC_TRAILER {
            out.truncate(out.len() - 4);
        } else {
            return Err(anyhow!(
                "deflate sync-flush did not end with 00 00 ff ff (len={})",
                out.len()
            ));
        }
        // Emit in MAX_DATA_COUNT-sized DEFLATED_DATA frames.
        let mut pos = 0;
        while pos < out.len() {
            let n = (out.len() - pos).min(MAX_DATA_COUNT);
            let hi = (n >> 8) as u8;
            let lo = (n & 0xff) as u8;
            self.inner.write_all(&[DEFLATED_DATA + hi, lo])?;
            self.inner.write_all(&out[pos..pos + n])?;
            pos += n;
        }
        self.pending_in.clear();
        self.have_unflushed_input = false;
        Ok(())
    }

    /// Feed matched-block bytes through deflate for dictionary sync; discard output.
    fn feed_dict_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let mut in_pos = 0;
        // Drive bytes through deflate(NoFlush). Output is discarded.
        while in_pos < bytes.len() {
            let mut tmp = vec![0u8; 64 * 1024];
            let before_in = self.deflate.total_in();
            let before_out = self.deflate.total_out();
            let _ = self
                .deflate
                .compress(&bytes[in_pos..], &mut tmp, FlushCompress::None)
                .map_err(|e| anyhow!("deflate dict-feed failed: {:?}", e))?;
            let consumed = (self.deflate.total_in() - before_in) as usize;
            let produced = (self.deflate.total_out() - before_out) as usize;
            if consumed == 0 && produced == 0 {
                return Err(anyhow!("deflate dict-feed stalled"));
            }
            in_pos += consumed;
        }
        // Issue a Sync flush so matched bytes are committed; discard output.
        // Single call with a large output buffer (deflate's sync output is
        // bounded by ~AVAIL_OUT_SIZE(input_len) which is at most input_len*1.001+16).
        let mut tmp = vec![0u8; bytes.len() + 256];
        let _ = self
            .deflate
            .compress(&[], &mut tmp, FlushCompress::Sync)
            .map_err(|e| anyhow!("deflate dict-sync drain failed: {:?}", e))?;
        Ok(())
    }
}

// ─── Reader ───────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum RxState {
    Init,
    Idle,
    Inflating,
    Inflated,
    Running,
}

pub struct DeflatedTokenReader<R: Read> {
    inner: R,
    inflate: Decompress,
    state: RxState,
    rx_token: i32,
    rx_run: i32,
    /// Saved flag byte from previous call (for handling pending DEFLATED_DATA before token byte).
    saved_flag: Option<u8>,
    /// Compressed-input buffer.
    cbuf: Vec<u8>,
    /// Decompressed-output buffer (drained between calls).
    dbuf: Vec<u8>,
    dbuf_pos: usize,
    /// True after we've seen a DEFLATED_DATA frame; false until then.
    have_inflated: bool,
}

impl<R: Read> DeflatedTokenReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            inflate: Decompress::new(false),
            state: RxState::Init,
            rx_token: 0,
            rx_run: 0,
            saved_flag: None,
            cbuf: Vec::new(),
            dbuf: vec![0u8; 64 * 1024],
            dbuf_pos: 0,
            have_inflated: false,
        }
    }

    pub fn read_token(&mut self) -> Result<Option<Token>> {
        loop {
            match self.state {
                RxState::Init => {
                    self.state = RxState::Idle;
                    self.rx_token = 0;
                }
                RxState::Idle | RxState::Inflated => {
                    let flag = match self.saved_flag.take() {
                        Some(f) => f,
                        None => {
                            let mut b = [0u8; 1];
                            self.inner.read_exact(&mut b)?;
                            b[0]
                        }
                    };
                    if (flag & 0xC0) == DEFLATED_DATA {
                        let lo = {
                            let mut b = [0u8; 1];
                            self.inner.read_exact(&mut b)?;
                            b[0] as usize
                        };
                        let n = (((flag & 0x3f) as usize) << 8) + lo;
                        self.cbuf.resize(n, 0);
                        self.inner.read_exact(&mut self.cbuf)?;
                        self.state = RxState::Inflating;
                        self.have_inflated = true;
                        continue;
                    }
                    // Non-DEFLATED_DATA flag arrived. If we just finished a run of
                    // DEFLATED_DATA frames (state == Inflated), we may still have
                    // pending decompressed output and must inject the synthetic
                    // 00 00 ff ff trailer before decoding the saved flag.
                    if self.state == RxState::Inflated {
                        // Drain remaining inflate output (no input).
                        let dbuf_cap = self.dbuf.len();
                        let before_in = self.inflate.total_in();
                        let before_out = self.inflate.total_out();
                        let _ = self.inflate.decompress(
                            &[],
                            &mut self.dbuf[..dbuf_cap],
                            FlushDecompress::Sync,
                        )?;
                        let produced =
                            (self.inflate.total_out() - before_out) as usize;
                        let _ = self.inflate.total_in() - before_in;
                        if produced > 0 {
                            self.dbuf_pos = 0;
                            self.dbuf.truncate(produced);
                            // restore capacity of underlying alloc by re-extending later
                            // Save flag for next loop.
                            self.saved_flag = Some(flag);
                            // Emit the just-inflated bytes as a literal token now.
                            let lit = self.dbuf.clone();
                            // Reset dbuf to working size.
                            self.dbuf = vec![0u8; 64 * 1024];
                            self.dbuf_pos = 0;
                            return Ok(Some(Token::Literal(lit)));
                        }
                        // No more pending output; inject synthetic trailer.
                        self.cbuf.clear();
                        self.cbuf.extend_from_slice(&SYNC_TRAILER);
                        let dbuf_cap = self.dbuf.len();
                        let before_in = self.inflate.total_in();
                        let _ = self.inflate.decompress(
                            &self.cbuf,
                            &mut self.dbuf[..dbuf_cap],
                            FlushDecompress::Sync,
                        )?;
                        let consumed_t =
                            (self.inflate.total_in() - before_in) as usize;
                        if consumed_t != 4 {
                            return Err(anyhow!(
                                "synthetic trailer not consumed (consumed={})",
                                consumed_t
                            ));
                        }
                        self.state = RxState::Idle;
                    }
                    if flag == END_FLAG {
                        self.state = RxState::Init;
                        self.have_inflated = false;
                        return Ok(None);
                    }
                    // Decode token flag.
                    let mut f = flag;
                    if f & TOKEN_REL != 0 {
                        self.rx_token += (f & 0x3f) as i32;
                        f >>= 6;
                    } else {
                        self.rx_token = read_int(&mut self.inner)?;
                        if self.rx_token < 0 {
                            return Err(anyhow!("invalid token in compressed stream"));
                        }
                    }
                    if f & 1 != 0 {
                        // run code: 2-byte LE run count of EXTRA tokens.
                        let mut bb = [0u8; 2];
                        self.inner.read_exact(&mut bb)?;
                        self.rx_run = (bb[0] as i32) | ((bb[1] as i32) << 8);
                        self.state = RxState::Running;
                    }
                    return Ok(Some(Token::BlockMatch(self.rx_token)));
                }
                RxState::Inflating => {
                    let dbuf_cap = self.dbuf.len();
                    let before_in = self.inflate.total_in();
                    let before_out = self.inflate.total_out();
                    let _status = self.inflate.decompress(
                        &self.cbuf,
                        &mut self.dbuf[..dbuf_cap],
                        FlushDecompress::None,
                    )?;
                    let consumed = (self.inflate.total_in() - before_in) as usize;
                    let produced = (self.inflate.total_out() - before_out) as usize;
                    // Trim cbuf by consumed bytes.
                    self.cbuf.drain(..consumed);
                    if self.cbuf.is_empty() {
                        self.state = RxState::Inflated;
                    }
                    if produced > 0 {
                        let lit = self.dbuf[..produced].to_vec();
                        return Ok(Some(Token::Literal(lit)));
                    }
                    // No output yet — loop to re-enter Inflated/Idle if input drained.
                }
                RxState::Running => {
                    self.rx_token += 1;
                    self.rx_run -= 1;
                    if self.rx_run == 0 {
                        self.state = RxState::Idle;
                    }
                    return Ok(Some(Token::BlockMatch(self.rx_token)));
                }
            }
        }
    }

    /// After a BlockMatch token is returned, the caller must invoke this with
    /// the matched bytes from the basis file so the inflate dictionary stays
    /// in sync with the sender.
    pub fn see_block(&mut self, mut bytes: &[u8]) -> Result<()> {
        // Inject a fake stored-block header `00 LL LL ~LL ~LL` per token.c:637-676,
        // then feed the bytes through inflate(Sync).
        while !bytes.is_empty() {
            let blklen = bytes.len().min(0xffff);
            let hdr = [
                0x00,
                (blklen & 0xff) as u8,
                ((blklen >> 8) & 0xff) as u8,
                !((blklen & 0xff) as u8),
                !(((blklen >> 8) & 0xff) as u8),
            ];
            // Feed header.
            let dbuf_cap = self.dbuf.len();
            let before_in = self.inflate.total_in();
            let _ = self.inflate.decompress(
                &hdr,
                &mut self.dbuf[..dbuf_cap],
                FlushDecompress::Sync,
            )?;
            let consumed = (self.inflate.total_in() - before_in) as usize;
            if consumed != 5 {
                return Err(anyhow!(
                    "see_block: header not fully consumed ({} of 5)",
                    consumed
                ));
            }
            // Feed the block bytes.
            let mut chunk = &bytes[..blklen];
            while !chunk.is_empty() {
                let dbuf_cap = self.dbuf.len();
                let before_in = self.inflate.total_in();
                let _ = self.inflate.decompress(
                    chunk,
                    &mut self.dbuf[..dbuf_cap],
                    FlushDecompress::Sync,
                )?;
                let c = (self.inflate.total_in() - before_in) as usize;
                if c == 0 {
                    return Err(anyhow!("see_block: inflate stalled"));
                }
                chunk = &chunk[c..];
            }
            bytes = &bytes[blklen..];
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn drain_reader<R: Read>(mut tr: DeflatedTokenReader<R>, basis_lookup: impl Fn(i32) -> Vec<u8>) -> Vec<Token> {
        let mut out = Vec::new();
        while let Some(tok) = tr.read_token().unwrap() {
            match &tok {
                Token::BlockMatch(idx) => {
                    let block = basis_lookup(*idx);
                    if !block.is_empty() {
                        tr.see_block(&block).unwrap();
                    }
                    out.push(tok);
                }
                Token::Literal(_) => out.push(tok),
            }
        }
        out
    }

    #[test]
    fn empty_round_trip() {
        let mut buf = Vec::new();
        let w = DeflatedTokenWriter::new(&mut buf);
        w.finish().unwrap();
        // Must be exactly END_FLAG.
        assert_eq!(buf, vec![END_FLAG]);
        let tr = DeflatedTokenReader::new(Cursor::new(&buf));
        let toks = drain_reader(tr, |_| vec![]);
        assert!(toks.is_empty());
    }

    #[test]
    fn small_literal_round_trip() {
        let payload = b"hello, world".to_vec();
        let mut buf = Vec::new();
        let mut w = DeflatedTokenWriter::new(&mut buf);
        w.send_literal(&payload).unwrap();
        w.finish().unwrap();

        let tr = DeflatedTokenReader::new(Cursor::new(&buf));
        let toks = drain_reader(tr, |_| vec![]);
        let assembled: Vec<u8> = toks
            .iter()
            .filter_map(|t| if let Token::Literal(b) = t { Some(b.clone()) } else { None })
            .flatten()
            .collect();
        assert_eq!(assembled, payload);
    }

    #[test]
    fn large_literal_multi_frame() {
        // Pseudo-random 64 KiB → forces multiple DEFLATED_DATA frames.
        let mut payload = Vec::with_capacity(64 * 1024);
        let mut x: u32 = 0xdeadbeef;
        for _ in 0..64 * 1024 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            payload.push((x >> 24) as u8);
        }
        let mut buf = Vec::new();
        let mut w = DeflatedTokenWriter::new(&mut buf);
        w.send_literal(&payload).unwrap();
        w.finish().unwrap();

        let tr = DeflatedTokenReader::new(Cursor::new(&buf));
        let toks = drain_reader(tr, |_| vec![]);
        let assembled: Vec<u8> = toks
            .iter()
            .filter_map(|t| if let Token::Literal(b) = t { Some(b.clone()) } else { None })
            .flatten()
            .collect();
        assert_eq!(assembled, payload);
    }

    #[test]
    fn match_then_literal_dict_sync() {
        // Test that compressed literals after a block match still inflate correctly.
        // Block 0 = b"BLOCK_AAAAA"; literal references repeated content.
        let block_data = b"BLOCK_AAAAA".to_vec();
        let lit_a = b"prefix-".to_vec();
        let lit_b = b"-BLOCK_AAAAA-suffix".to_vec(); // contains the matched bytes

        let mut buf = Vec::new();
        let mut w = DeflatedTokenWriter::new(&mut buf);
        w.send_literal(&lit_a).unwrap();
        w.send_block_match(0, &block_data).unwrap();
        w.send_literal(&lit_b).unwrap();
        w.finish().unwrap();

        let mut tr = DeflatedTokenReader::new(Cursor::new(&buf));
        let mut got: Vec<Token> = Vec::new();
        while let Some(tok) = tr.read_token().unwrap() {
            match &tok {
                Token::BlockMatch(idx) => {
                    assert_eq!(*idx, 0);
                    tr.see_block(&block_data).unwrap();
                    got.push(tok);
                }
                Token::Literal(_) => got.push(tok),
            }
        }
        // Reconstruct sequence: literals + block 0 in order.
        let mut reconstructed = Vec::new();
        for t in &got {
            match t {
                Token::Literal(b) => reconstructed.extend_from_slice(b),
                Token::BlockMatch(_) => reconstructed.extend_from_slice(&block_data),
            }
        }
        let mut expected = Vec::new();
        expected.extend_from_slice(&lit_a);
        expected.extend_from_slice(&block_data);
        expected.extend_from_slice(&lit_b);
        assert_eq!(reconstructed, expected);
    }

    #[test]
    fn consecutive_matches_run() {
        // Matches 5,6,7,8 → should encode as a run.
        let blk = b"X".repeat(8);
        let mut buf = Vec::new();
        let mut w = DeflatedTokenWriter::new(&mut buf);
        for i in 5..=8 {
            w.send_block_match(i, &blk).unwrap();
        }
        w.finish().unwrap();

        let mut tr = DeflatedTokenReader::new(Cursor::new(&buf));
        let mut indices = Vec::new();
        while let Some(tok) = tr.read_token().unwrap() {
            if let Token::BlockMatch(i) = tok {
                indices.push(i);
                tr.see_block(&blk).unwrap();
            }
        }
        assert_eq!(indices, vec![5, 6, 7, 8]);
    }

    #[test]
    fn nonconsecutive_matches_no_run() {
        let blk = b"X".repeat(4);
        let mut buf = Vec::new();
        let mut w = DeflatedTokenWriter::new(&mut buf);
        w.send_block_match(2, &blk).unwrap();
        w.send_block_match(10, &blk).unwrap();
        w.finish().unwrap();

        let mut tr = DeflatedTokenReader::new(Cursor::new(&buf));
        let mut indices = Vec::new();
        while let Some(tok) = tr.read_token().unwrap() {
            if let Token::BlockMatch(i) = tok {
                indices.push(i);
                tr.see_block(&blk).unwrap();
            }
        }
        assert_eq!(indices, vec![2, 10]);
    }
}
