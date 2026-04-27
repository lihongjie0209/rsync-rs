//! Receiver pipeline stage — applies delta tokens to reconstruct files.
//!
//! Mirrors `receiver.c` from the C rsync implementation.

#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};

use crate::delta::match_blocks::read_sum_head;
use crate::delta::token::{Token, TokenReader};
use crate::io::varint::read_int;
use crate::pipeline::apply_delta;
use crate::protocol::constants::{CsumType, NDX_DONE};
use crate::protocol::types::{FileList, Stats, SumHead};

// ── Public struct ─────────────────────────────────────────────────────────────

/// Receiver pipeline stage — mirrors `receive_files()` in `receiver.c`.
pub struct Receiver<R: Read> {
    reader: R,
    pub stats: Stats,
}

impl<R: Read> Receiver<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, stats: Stats::default() }
    }

    /// Run the receiver loop.
    ///
    /// Reads (file_index, tokens) pairs from the multiplex stream.
    /// For each file:
    ///   1. Read file index (read_int) — NDX_DONE (-1) ends loop
    ///   2. Read SumHead (to know block_len for reconstruction)
    ///   3. Read tokens via TokenReader until EOF token
    ///   4. Apply tokens to basis file (if exists) to reconstruct new file
    ///   5. Write reconstructed file to dest_dir/filename atomically
    ///   6. Apply metadata (mtime, chmod, chown)
    ///
    /// Returns accumulated stats.
    pub fn run(
        &mut self,
        flist: &FileList,
        dest_dir: &Path,
        _csum_type: CsumType,
        _protocol: u32,
    ) -> Result<Stats> {
        loop {
            let ndx = read_int(&mut self.reader)?;
            if ndx == NDX_DONE {
                break;
            }

            let fi = flist
                .get_by_ndx(ndx)
                .ok_or_else(|| anyhow::anyhow!("receiver: invalid file index {ndx}"))?;

            // Read the block-checksum header from the sender.
            let head = read_sum_head(&mut self.reader)?;

            // Collect all tokens for this file.
            let mut token_reader = TokenReader::new(&mut self.reader);
            let mut tokens: Vec<Token> = Vec::new();
            while let Some(tok) = token_reader.read_token()? {
                tokens.push(tok);
            }

            // Read the existing basis file (if present) for block matching.
            let dest_path = dest_dir.join(fi.path());
            let basis = if dest_path.exists() {
                fs::read(&dest_path).unwrap_or_default()
            } else {
                Vec::new()
            };

            // Reconstruct file content from the token stream.
            let new_data = apply_tokens(tokens, &basis, &head);
            self.stats.total_written += new_data.len() as i64;

            // Only write regular files; directories are created below.
            if fi.is_regular() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create_dir_all {:?}", parent))?;
                }

                // Write atomically: temp file → rename.
                let file_name = dest_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                let tmp_path = dest_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(format!(".{}.{}.tmp", file_name, std::process::id()));

                {
                    let mut tmp = fs::File::create(&tmp_path)
                        .with_context(|| format!("create temp file {:?}", tmp_path))?;
                    tmp.write_all(&new_data)
                        .with_context(|| format!("write temp file {:?}", tmp_path))?;
                }

                fs::rename(&tmp_path, &dest_path)
                    .with_context(|| format!("rename {:?} -> {:?}", tmp_path, dest_path))?;

                apply_metadata(&dest_path, fi)?;
                self.stats.xferred_files += 1;
                self.stats.created_files += 1;
            } else if fi.is_dir() {
                fs::create_dir_all(&dest_path)
                    .with_context(|| format!("create_dir_all {:?}", dest_path))?;
            }
        }

        Ok(self.stats.clone())
    }
}

// ── Metadata helpers ──────────────────────────────────────────────────────────

/// Apply file metadata (permissions, mtime) after writing.
fn apply_metadata(path: &Path, fi: &crate::protocol::types::FileInfo) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(fi.mode & 0o7777);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("set_permissions {:?}", path))?;
    }

    set_mtime(path, fi.modtime)?;

    Ok(())
}

/// Set the modification time of a file.
#[cfg(unix)]
fn set_mtime(path: &Path, mtime: i64) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).context("null byte in path")?;
    let times = [
        libc::timespec { tv_sec: mtime as libc::time_t, tv_nsec: 0 },
        libc::timespec { tv_sec: mtime as libc::time_t, tv_nsec: 0 },
    ];
    let ret = unsafe {
        libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0)
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "utimensat {:?}: {}",
            path,
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn set_mtime(_path: &Path, _mtime: i64) -> Result<()> {
    // mtime setting is not supported on non-Unix platforms in this implementation.
    Ok(())
}

// ── apply_tokens (public API) ─────────────────────────────────────────────────

/// Apply a token stream to reconstruct file data.
///
/// - `Token::Literal(data)` → append `data` verbatim.
/// - `Token::BlockMatch(idx)` → copy block `idx` from `basis`
///   (offset = `idx * head.blength`; last block uses `head.remainder` if set).
pub fn apply_tokens(tokens: Vec<Token>, basis: &[u8], head: &SumHead) -> Vec<u8> {
    // Delegate to apply_delta which already implements this correctly.
    apply_delta(&tokens, basis, head)
}

// ── Generator + Receiver: server-receiver (push) and client-pull paths ───────

/// Drive a full server-receiver session.
///
/// Mirrors the C generator+receiver pair: for each file in `flist` that
/// needs an update, write `NDX + iflags + sum_head + sum_bufs` to the
/// sender, then read back `NDX + iflags + sum_head + tokens + file_sum`
/// and reconstruct the destination file.
pub fn run_server_receiver<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    flist: &FileList,
    dest_dir: &Path,
    csum_type: CsumType,
    protocol: u32,
    checksum_seed: i32,
    use_zlib: bool,
    inplace: bool,
    itemize: bool,
) -> Result<Stats> {
    use crate::delta::match_blocks::{write_sum_bufs, write_sum_head};
    use crate::fileops::slurp_file;
    use crate::io::varint::{read_ndx, read_shortint, write_int, write_ndx, write_shortint};
    use crate::protocol::constants::ITEM_TRANSFER;

    let mut stats = Stats::default();
    let sum_len = super::csum_sum_len(csum_type);
    let strong_ct = super::csum_type_to_checksum_type(csum_type);

    // First, create any directories present in flist.
    for fi in flist.files.iter() {
        if fi.is_dir() {
            let p = dest_dir.join(fi.path());
            let _ = fs::create_dir_all(&p);
        }
    }
    crate::rdebug!("[srv-recv] flist has {} entries, scanning regular files", flist.files.len());

    // Phase 0: walk flist and ask for each regular file.
    for (i, fi) in flist.files.iter().enumerate() {
        if !fi.is_regular() {
            continue;
        }
        let dest_path = dest_dir.join(fi.path());

        // Quick check: skip if size + mtime match (unless --checksum, which is
        // not plumbed through here yet — always transfer for now if file is
        // missing/different).
        let needs = match fs::metadata(&dest_path) {
            Err(_) => true,
            Ok(m) => {
                let mtime_match;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    mtime_match = m.mtime() == fi.modtime;
                }
                #[cfg(not(unix))]
                {
                    mtime_match = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64 == fi.modtime)
                        .unwrap_or(false);
                }
                m.len() != fi.size as u64 || !mtime_match
            }
        };
        if !needs {
            continue;
        }

        let ndx = flist.ndx_start + i as i32;
        crate::rdebug!("[srv-recv] requesting file ndx={} path={:?}", ndx, fi.path());
        if protocol >= 30 {
            write_ndx(&mut writer, ndx)?;
        } else {
            write_int(&mut writer, ndx)?;
        }
        if protocol >= 29 {
            write_shortint(&mut writer, ITEM_TRANSFER as u16)?;
        }

        // Build basis sum_head + sum_bufs.
        let (head, sums, basis_data) = if let Ok(data) = slurp_file(&dest_path) {
            let blength = crate::util::block_len_for_file(data.len() as i64);
            let count = crate::util::sum_count_for_file(data.len() as i64, blength);
            let remainder = crate::util::remainder_for_file(data.len() as i64, blength);
            let h = SumHead { count, blength, s2length: sum_len, remainder };
            let s = crate::pipeline::sender::compute_sum_bufs(
                &data,
                &h,
                csum_type,
                checksum_seed as u32,
                false,
            );
            (h, s, data)
        } else {
            (
                SumHead { count: 0, blength: 0, s2length: sum_len, remainder: 0 },
                Vec::new(),
                Vec::new(),
            )
        };
        write_sum_head(&mut writer, &head)?;
        write_sum_bufs(&mut writer, &sums)?;
        writer.flush().ok();
        crate::rdebug!("[srv-recv] sent sum_head count={} blen={}, awaiting response", head.count, head.blength);

        // Read sender's response: NDX + iflags + sum_head + tokens + file_sum.
        let _echo_ndx = if protocol >= 30 {
            read_ndx(&mut reader)?
        } else {
            read_int(&mut reader)?
        };
        let _echo_iflags = if protocol >= 29 {
            read_shortint(&mut reader)? as u32
        } else {
            ITEM_TRANSFER
        };
        let echo_head = read_sum_head(&mut reader)?;
        let new_data = if use_zlib {
            let mut tr = crate::delta::DeflatedTokenReader::new(&mut reader);
            let mut out: Vec<u8> = Vec::new();
            let blength = echo_head.blength as usize;
            let count = echo_head.count as usize;
            while let Some(tok) = tr.read_token()? {
                match tok {
                    Token::Literal(data) => out.extend_from_slice(&data),
                    Token::BlockMatch(idx) => {
                        let i = idx as usize;
                        let offset = i * blength;
                        let is_last = count > 0 && i == count - 1;
                        let block_len = if is_last && echo_head.remainder != 0 {
                            echo_head.remainder as usize
                        } else {
                            blength
                        };
                        let end = (offset + block_len).min(basis_data.len());
                        let block = if offset < basis_data.len() {
                            &basis_data[offset..end]
                        } else {
                            &[][..]
                        };
                        out.extend_from_slice(block);
                        tr.see_block(block)?;
                    }
                }
            }
            out
        } else {
            let mut tr = TokenReader::new(&mut reader);
            let mut tokens: Vec<Token> = Vec::new();
            while let Some(tok) = tr.read_token()? {
                tokens.push(tok);
            }
            apply_delta(&tokens, &basis_data, &echo_head)
        };
        let mut file_sum = vec![0u8; strong_ct.digest_len()];
        if !file_sum.is_empty() {
            reader.read_exact(&mut file_sum)?;
        }

        // Apply delta + write file.
        if let Some(parent) = dest_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let dest_existed = dest_path.exists();
        if inplace {
            // Write directly to destination — no temp file, no rename.
            let mut f = fs::File::create(&dest_path)?;
            f.write_all(&new_data)?;
        } else {
            let file_name = dest_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            let tmp_path = dest_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!(".{}.{}.tmp", file_name, std::process::id()));
            {
                let mut tmp = fs::File::create(&tmp_path)?;
                tmp.write_all(&new_data)?;
            }
            fs::rename(&tmp_path, &dest_path)?;
        }
        apply_metadata(&dest_path, fi)?;

        if itemize {
            use crate::protocol::constants::*;
            let mode = fi.mode as u32;
            let iflags = if dest_existed {
                ITEM_TRANSFER | ITEM_REPORT_SIZE | ITEM_REPORT_TIME
            } else {
                ITEM_IS_NEW | ITEM_TRANSFER
            };
            let prefix = crate::util::iflags_to_str(iflags, mode, false);
            let path_str = fi.path();
            eprintln!("{} {}", prefix, path_str);
        }

        stats.num_files += 1;
        stats.xferred_files += 1;
        stats.created_files += 1;
        stats.total_written += new_data.len() as i64;
    }

    // Symlinks: just create them locally (no data transfer).
    for fi in flist.files.iter() {
        if fi.is_symlink() {
            let target = fi.link_target.as_deref().unwrap_or("");
            let p = dest_dir.join(fi.path());
            let _ = fs::remove_file(&p);
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(target, &p);
            }
            #[cfg(windows)]
            {
                let _ = std::os::windows::fs::symlink_file(target, &p);
            }
        }
    }

    // ── Phase / goodbye exchange ──────────────────────────────────────────
    // Mirrors the receiver+generator side of C's exit dance against the
    // sender (sender.c:send_files() main loop and main.c::read_final_goodbye).
    //
    // Receiver writes:                       Sender (peer) does:
    //   NDX_DONE  (end of phase 0)   ─►      reads, phase=1, echoes NDX_DONE
    //                                ◄─      (we read it)
    //   NDX_DONE  (end of phase 1)   ─►      reads, phase=2, echoes NDX_DONE
    //                                ◄─      (we read it)
    //   NDX_DONE  (end of phase 2)   ─►      reads, phase=MAX_PHASE → break
    //                                ◄─      (sender.c:464) writes final NDX_DONE
    //   NDX_DONE  (final goodbye)    ─►      read_final_goodbye reads it
    //   For proto >= 31 (am_sender): sender writes another NDX_DONE
    //                                ◄─      (we read it)
    //                                        then reads another from us
    //   NDX_DONE  (proto-31 second)  ─►      read_final_goodbye second read

    let max_phase = if protocol >= 29 { 2 } else { 1 };
    // Phase boundaries: write NDX_DONE for each phase up to max_phase, reading
    // the sender's echo after each.
    for _ in 0..max_phase {
        if protocol >= 30 { write_ndx(&mut writer, NDX_DONE)?; } else { write_int(&mut writer, NDX_DONE)?; }
        writer.flush().ok();
        let _ = if protocol >= 30 { read_ndx(&mut reader)? } else { read_int(&mut reader)? };
    }
    // Final phase-end write that breaks the sender out of its main loop.
    if protocol >= 30 { write_ndx(&mut writer, NDX_DONE)?; } else { write_int(&mut writer, NDX_DONE)?; }
    writer.flush().ok();

    // C sender is now in main.c::read_final_goodbye and expects another NDX_DONE.
    if protocol >= 30 { write_ndx(&mut writer, NDX_DONE)?; } else { write_int(&mut writer, NDX_DONE)?; }
    writer.flush().ok();
    if protocol >= 31 {
        // For proto >= 31 + am_sender, sender writes NDX_DONE then reads another.
        let _ = if protocol >= 30 { read_ndx(&mut reader)? } else { read_int(&mut reader)? };
        if protocol >= 30 { write_ndx(&mut writer, NDX_DONE)?; } else { write_int(&mut writer, NDX_DONE)?; }
        writer.flush().ok();
    }
    // Drain sender.c:464 final NDX_DONE if present (best-effort).
    let _ = if protocol >= 30 { read_ndx(&mut reader).ok() } else { read_int(&mut reader).ok() };

    Ok(stats)
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::token::TokenWriter;
    use crate::io::varint::write_int;
    use crate::protocol::constants::NDX_DONE;
    use crate::protocol::types::FileList;
    use std::io::Cursor;

    fn make_head(count: i32, blength: i32, remainder: i32) -> SumHead {
        SumHead { count, blength, s2length: 16, remainder }
    }

    #[test]
    fn apply_tokens_all_literals() {
        let tokens = vec![
            Token::Literal(b"hello".to_vec()),
            Token::Literal(b" world".to_vec()),
        ];
        let head = make_head(0, 700, 0);
        assert_eq!(apply_tokens(tokens, b"ignored", &head), b"hello world");
    }

    #[test]
    fn apply_tokens_block_match() {
        let basis = b"abcdefgh";
        let head = make_head(2, 4, 0);
        let tokens = vec![Token::BlockMatch(1), Token::BlockMatch(0)];
        assert_eq!(apply_tokens(tokens, basis, &head), b"efghabcd");
    }

    #[test]
    fn apply_tokens_mixed() {
        let basis = b"ABCDE";
        let head = make_head(2, 3, 2);
        let tokens = vec![
            Token::BlockMatch(0),
            Token::Literal(b"XY".to_vec()),
            Token::BlockMatch(1),
        ];
        assert_eq!(apply_tokens(tokens, basis, &head), b"ABCXYDE");
    }

    #[test]
    fn apply_tokens_empty() {
        let head = make_head(0, 700, 0);
        assert!(apply_tokens(vec![], b"basis", &head).is_empty());
    }

    /// Full round-trip: TokenWriter → wire → TokenReader → apply_tokens.
    #[test]
    fn receiver_token_roundtrip() {
        let mut wire = Vec::new();
        let mut tw = TokenWriter::new(&mut wire);
        tw.send_literal(b"foo").unwrap();
        tw.finish().unwrap();

        let head = make_head(0, 700, 0);
        let mut tr = TokenReader::new(Cursor::new(&wire));
        let mut tokens = Vec::new();
        while let Some(tok) = tr.read_token().unwrap() {
            tokens.push(tok);
        }
        assert_eq!(apply_tokens(tokens, b"", &head), b"foo");
    }

    /// Receiver::run should return immediately when the first int is NDX_DONE.
    #[test]
    fn receiver_run_ndx_done_immediately() {
        let mut wire = Vec::new();
        write_int(&mut wire, NDX_DONE).unwrap();

        let flist = FileList::new();
        let dest = std::env::temp_dir();
        let mut recv = Receiver::new(Cursor::new(wire));
        let stats = recv.run(&flist, &dest, CsumType::Md5, 31).unwrap();
        assert_eq!(stats.xferred_files, 0);
    }
}
