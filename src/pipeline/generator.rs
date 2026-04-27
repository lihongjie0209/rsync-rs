//! Generator pipeline stage — Rust port of `generator.c`.
//!
//! The generator iterates the file list, decides which files need updating
//! (size/mtime check), writes file indices and block checksums to the sender,
//! and sends `NDX_DONE` when finished.
//!
//! It also exposes `apply_delta`, which the receiver calls to reconstruct a
//! file from a token stream and a basis.

#![allow(dead_code)]

use std::io::{Read, Write};

use anyhow::Result;

use crate::delta::match_blocks::{write_sum_bufs, write_sum_head};
use crate::delta::token::Token;
use crate::fileops::slurp_file;
use crate::io::varint::write_int;
use crate::protocol::constants::{CsumType, NDX_DONE};
use crate::protocol::types::{FileInfo, FileList, Stats, SumHead};
use crate::util::{block_len_for_file, remainder_for_file, sum_count_for_file};

// ── Public struct ─────────────────────────────────────────────────────────────

/// Generator pipeline — mirrors the generator loop in `generator.c`.
pub struct Generator<R: Read, W: Write> {
    /// Reads tokens/messages back from the sender (for verification / redo).
    reader: R,
    /// Sends file indices and block checksums to the sender.
    writer: W,
    pub stats: Stats,
}

impl<R: Read, W: Write> Generator<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Generator { reader, writer, stats: Stats::default() }
    }

    /// Run the generator loop.
    ///
    /// For each regular file in `flist` that needs updating:
    ///   1. Write the file's protocol index (`write_int`).
    ///   2. Write `SumHead` — zeroed if the file is new, real checksums
    ///      otherwise.
    ///   3. Write `SumBuf`s.
    ///
    /// After processing all files, writes `NDX_DONE` (`-1`).
    pub fn run(
        &mut self,
        flist: &FileList,
        dest_dir: &std::path::Path,
        csum_type: CsumType,
        protocol: u32,
        checksum_whole_file: bool,
    ) -> Result<()> {
        let sum_len = super::csum_sum_len(csum_type);

        for (i, fi) in flist.files.iter().enumerate() {
            if !fi.is_regular() {
                continue;
            }

            let should_transfer = checksum_whole_file || Self::needs_update(fi, dest_dir);
            if !should_transfer {
                continue;
            }

            let ndx = flist.ndx_start + i as i32;
            write_int(&mut self.writer, ndx)?;

            let dest_path = dest_dir.join(fi.path());

            // If file exists locally and we are not doing whole-file mode,
            // send real block checksums so the sender can compute a delta.
            // Otherwise send an empty SumHead (count=0) to request a full
            // file transfer.
            let sent_checksums = if !checksum_whole_file {
                self.try_send_real_checksums(&dest_path, fi.size, sum_len, csum_type, protocol)
                    .unwrap_or(false)
            } else {
                false
            };

            if !sent_checksums {
                // New file or whole-file mode: empty SumHead → sender sends all data as literals.
                let empty_head =
                    SumHead { count: 0, blength: 0, s2length: sum_len, remainder: 0 };
                write_sum_head(&mut self.writer, &empty_head)?;
                // No SumBufs to write.
            }

            self.stats.num_files += 1;
        }

        write_int(&mut self.writer, NDX_DONE)?;
        Ok(())
    }

    /// Attempt to read the destination file and write its block checksums.
    ///
    /// Returns `true` on success, `false` if the file cannot be read (e.g. it
    /// does not exist yet).
    fn try_send_real_checksums(
        &mut self,
        dest_path: &std::path::Path,
        file_size: i64,
        sum_len: i32,
        csum_type: CsumType,
        _protocol: u32,
    ) -> Result<bool> {
        let data = match slurp_file(dest_path) {
            Ok(d) => d,
            Err(_) => return Ok(false),
        };

        let head = make_sum_head(file_size, sum_len);
        let sums = crate::pipeline::sender::compute_sum_bufs(&data, &head, csum_type, 0, false);

        write_sum_head(&mut self.writer, &head)?;
        write_sum_bufs(&mut self.writer, &sums)?;

        self.stats.total_written += data.len() as i64;
        Ok(true)
    }

    /// Return `true` when `fi` needs to be (re)transferred to `dest_dir`.
    ///
    /// Quick check: the destination file is absent, has a different size, or
    /// has a different modification time.
    pub fn needs_update(fi: &FileInfo, dest_dir: &std::path::Path) -> bool {
        let dest_path = dest_dir.join(fi.path());
        match std::fs::metadata(&dest_path) {
            Err(_) => true, // file does not exist
            Ok(meta) => {
                if meta.len() != fi.size as u64 {
                    return true;
                }
                // Compare modification time.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if meta.mtime() != fi.modtime {
                        return true;
                    }
                }
                #[cfg(not(unix))]
                {
                    if let Ok(modified) = meta.modified() {
                        use std::time::UNIX_EPOCH;
                        if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                            if dur.as_secs() as i64 != fi.modtime {
                                return true;
                            }
                        }
                    }
                }
                false
            }
        }
    }
}

// ── Local helpers ─────────────────────────────────────────────────────────────

/// Build a `SumHead` for a file of `file_size` bytes.
fn make_sum_head(file_size: i64, sum_len: i32) -> SumHead {
    let blength = block_len_for_file(file_size);
    let count = sum_count_for_file(file_size, blength);
    let remainder = remainder_for_file(file_size, blength);
    SumHead { count, blength, s2length: sum_len, remainder }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Reconstruct a file by applying a token stream to `basis_data`.
///
/// Called by the receiver after collecting all tokens for a file:
/// - `Token::Literal(bytes)` → append bytes verbatim.
/// - `Token::BlockMatch(idx)` → copy the block at `idx * head.blength` from
///   `basis_data` (last block uses `head.remainder` if non-zero).
pub fn apply_delta(tokens: &[Token], basis_data: &[u8], head: &SumHead) -> Vec<u8> {
    let mut out = Vec::new();
    let blength = head.blength as usize;
    let count = head.count as usize;

    for token in tokens {
        match token {
            Token::Literal(data) => {
                out.extend_from_slice(data);
            }
            Token::BlockMatch(idx) => {
                let i = *idx as usize;
                let offset = i * blength;
                let is_last = count > 0 && i == count - 1;
                let block_len = if is_last && head.remainder != 0 {
                    head.remainder as usize
                } else {
                    blength
                };
                let end = (offset + block_len).min(basis_data.len());
                if offset < basis_data.len() {
                    out.extend_from_slice(&basis_data[offset..end]);
                }
            }
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{FileInfo, SumHead};
    use std::path::PathBuf;

    // ── apply_delta ───────────────────────────────────────────────────────────

    fn make_head(count: i32, blength: i32, remainder: i32) -> SumHead {
        SumHead { count, blength, s2length: 16, remainder }
    }

    #[test]
    fn apply_delta_all_literals() {
        let tokens = vec![Token::Literal(b"hello".to_vec()), Token::Literal(b" world".to_vec())];
        let basis = b"old content";
        let head = make_head(0, 700, 0);
        let result = apply_delta(&tokens, basis, &head);
        assert_eq!(result, b"hello world");
    }

    #[test]
    fn apply_delta_all_matches() {
        // basis = "abcdefgh" (8 bytes), blength = 4, count = 2
        let basis = b"abcdefgh";
        let head = make_head(2, 4, 0);
        // Block 0 → "abcd", block 1 → "efgh"
        let tokens = vec![Token::BlockMatch(1), Token::BlockMatch(0)];
        let result = apply_delta(&tokens, basis, &head);
        assert_eq!(result, b"efghabcd");
    }

    #[test]
    fn apply_delta_mixed() {
        // basis = "ABCDE" (5 bytes), blength = 3, count = 2, remainder = 2
        let basis = b"ABCDE";
        let head = make_head(2, 3, 2);
        // Block 0 → "ABC" (3 bytes), block 1 → "DE" (remainder=2)
        let tokens = vec![
            Token::BlockMatch(0),  // "ABC"
            Token::Literal(b"XY".to_vec()),
            Token::BlockMatch(1),  // "DE"
        ];
        let result = apply_delta(&tokens, basis, &head);
        assert_eq!(result, b"ABCXYDE");
    }

    #[test]
    fn apply_delta_empty_tokens() {
        let tokens: Vec<Token> = vec![];
        let result = apply_delta(&tokens, b"basis", &make_head(0, 700, 0));
        assert!(result.is_empty());
    }

    #[test]
    fn apply_delta_block_out_of_bounds() {
        // Block index beyond basis_data length → should not panic, just skip.
        let basis = b"AB"; // only 2 bytes
        let head = make_head(1, 700, 0);
        // Block 0 would start at offset 0 but block_len = 700 > len(basis).
        let tokens = vec![Token::BlockMatch(0)];
        let result = apply_delta(&tokens, basis, &head);
        // Should copy whatever is available (2 bytes).
        assert_eq!(result, b"AB");
    }

    // ── needs_update ──────────────────────────────────────────────────────────

    fn make_file_info(name: &str, size: i64, modtime: i64) -> FileInfo {
        FileInfo { name: name.to_string(), size, modtime, ..Default::default() }
    }

    fn write_temp_file(dir: &std::path::Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn needs_update_absent_file() {
        let dir = std::env::temp_dir();
        let fi = make_file_info("__rsync_rs_absent_file_12345__.txt", 10, 0);
        // File does not exist → must update.
        assert!(Generator::<std::io::Empty, std::io::Sink>::needs_update(&fi, &dir));
    }

    #[test]
    fn needs_update_matching_file() {
        let dir = std::env::temp_dir();
        let name = "__rsync_rs_match_test__.txt";
        let content = b"hello world";
        write_temp_file(&dir, name, content);

        let meta = std::fs::metadata(dir.join(name)).unwrap();
        #[cfg(unix)]
        let mtime = {
            use std::os::unix::fs::MetadataExt;
            meta.mtime()
        };
        #[cfg(not(unix))]
        let mtime = {
            use std::time::UNIX_EPOCH;
            meta.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        };

        let fi = make_file_info(name, content.len() as i64, mtime);
        let result = Generator::<std::io::Empty, std::io::Sink>::needs_update(&fi, &dir);
        let _ = std::fs::remove_file(dir.join(name));
        assert!(!result);
    }

    #[test]
    fn needs_update_size_mismatch() {
        let dir = std::env::temp_dir();
        let name = "__rsync_rs_size_mismatch__.txt";
        write_temp_file(&dir, name, b"hello");
        let fi = make_file_info(name, 999, 0); // wrong size
        let result = Generator::<std::io::Empty, std::io::Sink>::needs_update(&fi, &dir);
        let _ = std::fs::remove_file(dir.join(name));
        assert!(result);
    }
}
