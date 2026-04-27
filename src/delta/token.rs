//! Token encoding/decoding — uncompressed path from token.c.
//!
//! Wire format (simple_send_token / simple_recv_token):
//!   Positive N  → N literal bytes follow
//!   Zero        → end-of-file sentinel
//!   Negative -N → block match for 0-based block index (N - 1)

#![allow(dead_code)]

use std::io::{Read, Write};

use anyhow::Result;

use crate::io::varint::{read_int, write_int};

// Maximum chunk size that matches the C CHUNK_SIZE constant (32 KiB).
const CHUNK_SIZE: usize = 32 * 1024;

#[derive(Debug)]
pub enum Token {
    /// Raw byte payload.
    Literal(Vec<u8>),
    /// 0-based block index.
    BlockMatch(i32),
}

// ── Writer ────────────────────────────────────────────────────────────────────

pub struct TokenWriter<W: Write> {
    inner: W,
    literal_buf: Vec<u8>,
}

impl<W: Write> TokenWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner, literal_buf: Vec::new() }
    }

    /// Buffer literal bytes; they are sent in CHUNK_SIZE chunks on flush.
    pub fn send_literal(&mut self, data: &[u8]) -> Result<()> {
        self.literal_buf.extend_from_slice(data);
        // Eagerly flush complete chunks to bound memory usage.
        while self.literal_buf.len() >= CHUNK_SIZE {
            let chunk: Vec<u8> = self.literal_buf.drain(..CHUNK_SIZE).collect();
            write_int(&mut self.inner, chunk.len() as i32)?;
            self.inner.write_all(&chunk)?;
        }
        Ok(())
    }

    /// Flush any buffered literal data.
    pub fn flush_literals(&mut self) -> Result<()> {
        if !self.literal_buf.is_empty() {
            let buf = std::mem::take(&mut self.literal_buf);
            write_int(&mut self.inner, buf.len() as i32)?;
            self.inner.write_all(&buf)?;
        }
        Ok(())
    }

    /// Send a block-match reference (0-based index).
    /// Mirrors `write_int(f, -(token+1))` in simple_send_token.
    pub fn send_block_match(&mut self, block_idx: i32) -> Result<()> {
        self.flush_literals()?;
        write_int(&mut self.inner, -(block_idx + 1))?;
        Ok(())
    }

    /// Send the end-of-file sentinel (write_int 0).
    pub fn send_eof(&mut self) -> Result<()> {
        write_int(&mut self.inner, 0)?;
        Ok(())
    }

    /// Flush literals and write the EOF sentinel.
    pub fn finish(mut self) -> Result<()> {
        self.flush_literals()?;
        self.send_eof()
    }
}

// ── Reader ────────────────────────────────────────────────────────────────────

pub struct TokenReader<R: Read> {
    inner: R,
    /// Bytes of the current literal run still to be delivered.
    literal_remaining: i32,
}

impl<R: Read> TokenReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, literal_remaining: 0 }
    }

    /// Read the next token.
    ///
    /// Returns `Ok(None)` when the EOF sentinel (0) is received.
    pub fn read_token(&mut self) -> Result<Option<Token>> {
        loop {
            if self.literal_remaining > 0 {
                let n = self.literal_remaining.min(CHUNK_SIZE as i32) as usize;
                let mut buf = vec![0u8; n];
                self.inner.read_exact(&mut buf)?;
                self.literal_remaining -= n as i32;
                return Ok(Some(Token::Literal(buf)));
            }

            let i = read_int(&mut self.inner)?;
            match i.cmp(&0) {
                std::cmp::Ordering::Equal => return Ok(None),
                std::cmp::Ordering::Greater => {
                    // Literal data: i bytes follow (already chunked by sender).
                    let mut buf = vec![0u8; i as usize];
                    self.inner.read_exact(&mut buf)?;
                    return Ok(Some(Token::Literal(buf)));
                }
                std::cmp::Ordering::Less => {
                    // Block match: decode 0-based index.
                    let block_idx = (-i) - 1;
                    return Ok(Some(Token::BlockMatch(block_idx)));
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip(ops: Vec<(&[u8], Option<i32>)>) -> Vec<Token> {
        // ops: list of (literal_data, block_match_idx); use empty slice + Some(idx) for match
        let mut buf = Vec::new();
        let mut writer = TokenWriter::new(&mut buf);
        for (data, blk) in ops {
            if !data.is_empty() {
                writer.send_literal(data).unwrap();
            }
            if let Some(idx) = blk {
                writer.send_block_match(idx).unwrap();
            }
        }
        writer.finish().unwrap();

        let mut tokens = Vec::new();
        let mut reader = TokenReader::new(Cursor::new(&buf));
        while let Some(tok) = reader.read_token().unwrap() {
            tokens.push(tok);
        }
        tokens
    }

    #[test]
    fn eof_only() {
        let tokens = round_trip(vec![]);
        assert!(tokens.is_empty());
    }

    #[test]
    fn literal_round_trip() {
        let payload = b"hello, world";
        let tokens = round_trip(vec![(payload, None)]);
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            Token::Literal(data) => assert_eq!(data.as_slice(), payload),
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn block_match_round_trip() {
        let tokens = round_trip(vec![(b"", Some(0)), (b"", Some(7))]);
        assert!(matches!(&tokens[0], Token::BlockMatch(0)));
        assert!(matches!(&tokens[1], Token::BlockMatch(7)));
    }

    #[test]
    fn mixed_round_trip() {
        let tokens = round_trip(vec![(b"abc", None), (b"", Some(2)), (b"xyz", None)]);
        assert_eq!(tokens.len(), 3);
        match &tokens[0] {
            Token::Literal(d) => assert_eq!(d.as_slice(), b"abc"),
            _ => panic!(),
        }
        assert!(matches!(&tokens[1], Token::BlockMatch(2)));
        match &tokens[2] {
            Token::Literal(d) => assert_eq!(d.as_slice(), b"xyz"),
            _ => panic!(),
        }
    }

    #[test]
    fn large_literal_chunking() {
        // A payload larger than CHUNK_SIZE should still round-trip correctly.
        let payload: Vec<u8> = (0u8..=255).cycle().take(CHUNK_SIZE + 100).collect();
        let mut buf = Vec::new();
        let mut writer = TokenWriter::new(&mut buf);
        writer.send_literal(&payload).unwrap();
        writer.finish().unwrap();

        let mut assembled = Vec::new();
        let mut reader = TokenReader::new(Cursor::new(&buf));
        while let Some(tok) = reader.read_token().unwrap() {
            match tok {
                Token::Literal(d) => assembled.extend_from_slice(&d),
                _ => panic!("unexpected block match"),
            }
        }
        assert_eq!(assembled, payload);
    }
}
