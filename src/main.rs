pub mod checksum;
pub mod batch;
pub mod daemon;
pub mod delta;
pub mod fileops;
pub mod filter;
pub mod flist;
pub mod io;
pub mod options;
pub mod options_server;
pub mod uidlist;

pub mod pipeline;
pub mod protocol;
pub mod transport;
pub mod util;
#[path = "log.rs"]
pub mod log_mod;
pub mod progress;

use anyhow::{Context, Result};
use clap::Parser;
use std::io::Write as _;

use crate::io::varint::{read_int, read_varint, read_vstring, write_int, write_varint, write_vstring};
use crate::options::Options;
use crate::protocol::constants::{CF_INC_RECURSE, CF_SAFE_FLIST, CF_VARINT_FLIST_FLAGS, PROTOCOL_VERSION};
use crate::protocol::errcode::ExitCode;
use crate::protocol::types::Stats;
use crate::util::{big_num, comma_dnum, human_num};

// ── Version string ────────────────────────────────────────────────────────────

fn print_version() {
    print!(
        "rsync  version 3.4.2  protocol version {proto}\n\
         Copyright (C) 1996-2024 by Andrew Tridgell, Wayne Davison, and others.\n\
         Web site: https://rsync.samba.org/\n\
         Capabilities:\n\
         \t64-bit files, 64-bit inums, 64-bit timestamps, 64-bit symlinks,\n\
         \tsocketpairs, symlinks, symtimes, hardlinks, no hardlink-specials,\n\
         \tno hardlink-symlinks, IPv6, atimes, batchfiles, inplace, append,\n\
         \tACLs, xattrs, optional secluded-args, no iconv, no prealloc,\n\
         \tstop-at, no crtimes, file-flags\n\
         Optimizations:\n\
         \tno SIMD-roll, no asm-roll, openssl-crypto, no asm-MD5\n\
         Checksum list:\n\
         \tmd5 md4 none\n\
         Compress list:\n\
         \tzlibx zlib none\n\
         Daemon auth list:\n\
         \tmd5 md4\n\
         \n\
         rsync-rs is a Rust port of rsync.  See https://github.com/lihongjie0209/rsync-rs.\n\
         rsync comes with ABSOLUTELY NO WARRANTY.  This is free software, and you\n\
         are welcome to redistribute it under certain conditions.  See the GNU\n\
         General Public Licence for details.\n",
        proto = PROTOCOL_VERSION
    );
}

/// Print a help block whose layout mirrors C rsync's `help-rsync.h` —
/// the version banner, the `Usage:` block, then a flat `--long, -X
/// description` option list.  We only advertise options that rsync-rs
/// actually implements; flags accepted but not yet honoured are marked
/// `(stub)`, and entries unique to rsync-rs are tagged accordingly.
fn print_help() {
    print_version();
    print!(
        "\n\
         rsync is a file transfer program capable of efficient remote update\n\
         via a fast differencing algorithm.\n\
         \n\
         Usage: rsync [OPTION]... SRC [SRC]... DEST\n\
         \x20 or   rsync [OPTION]... SRC [SRC]... [USER@]HOST:DEST\n\
         \x20 or   rsync [OPTION]... SRC [SRC]... [USER@]HOST::DEST\n\
         \x20 or   rsync [OPTION]... SRC [SRC]... rsync://[USER@]HOST[:PORT]/DEST\n\
         \x20 or   rsync [OPTION]... [USER@]HOST:SRC [DEST]\n\
         \x20 or   rsync [OPTION]... [USER@]HOST::SRC [DEST]\n\
         \x20 or   rsync [OPTION]... rsync://[USER@]HOST[:PORT]/SRC [DEST]\n\
         The ':' usages connect via remote shell, while '::' & 'rsync://' usages connect\n\
         to an rsync daemon, and require SRC or DEST to start with a module name.\n\
         \n\
         Options\n"
    );
    // Each line: '--long, -X' (or just '--long') padded to col 25, then text.
    // Mirrors the column layout in C rsync's help-rsync.h.
    let lines: &[&str] = &[
        "--verbose, -v            increase verbosity",
        "--quiet, -q              suppress non-error messages",
        "--checksum, -c           skip based on checksum, not mod-time & size",
        "--archive, -a            archive mode is -rlptgoD (no -A,-X,-U,-N,-H)",
        "--recursive, -r          recurse into directories",
        "--relative, -R           use relative path names",
        "--no-implied-dirs        don't send implied dirs with --relative",
        "--backup, -b             make backups (see --suffix & --backup-dir)",
        "--backup-dir=DIR         make backups into hierarchy based in DIR",
        "--suffix=SUFFIX          backup suffix (default ~ w/o --backup-dir)",
        "--update, -u             skip files that are newer on the receiver",
        "--inplace                update destination files in-place",
        "--append                 append data onto shorter files",
        "--mkpath                 create destination's missing path components",
        "--links, -l              copy symlinks as symlinks",
        "--copy-links, -L         transform symlink into referent file/dir",
        "--copy-dirlinks, -k      transform symlink to dir into referent dir",
        "--keep-dirlinks, -K      treat symlinked dir on receiver as dir",
        "--hard-links, -H         preserve hard links",
        "--perms, -p              preserve permissions",
        "--executability, -E      preserve executability",
        "--acls, -A               preserve ACLs (stub: parsed, not applied)",
        "--xattrs, -X             preserve extended attributes (stub: parsed, not applied)",
        "--owner, -o              preserve owner (super-user only)",
        "--group, -g              preserve group",
        "--devices                preserve device files (super-user only)",
        "--specials               preserve special files",
        "-D                       same as --devices --specials",
        "--times, -t              preserve modification times",
        "--omit-dir-times, -O     omit directories from --times",
        "--omit-link-times        omit symlinks from --times",
        "--dry-run, -n            perform a trial run with no changes made",
        "--whole-file, -W         copy files whole (w/o delta-xfer algorithm)",
        "--no-whole-file          force the delta-xfer algorithm",
        "--checksum-choice=STR    choose the checksum algorithm",
        "--one-file-system        don't cross filesystem boundaries",
        "--rsh=COMMAND, -e        specify the remote shell to use",
        "--rsync-path=PROGRAM     specify the rsync to run on remote machine",
        "--ignore-existing        skip updating files that exist on receiver",
        "--ignore-non-existing    skip files that don't exist on receiver (rsync-rs)",
        "--remove-source-files    sender removes synchronized files (non-dir)",
        "--delete                 delete extraneous files from dest dirs",
        "--delete-before          receiver deletes before xfer, not during",
        "--delete-during          receiver deletes during the transfer",
        "--delete-after           receiver deletes after transfer, not during",
        "--delete-excluded        also delete excluded files from dest dirs",
        "--ignore-errors          delete even if there are I/O errors",
        "--force                  force deletion of dirs even if not empty",
        "--max-delete=NUM         don't delete more than NUM files",
        "--max-size=SIZE          don't transfer any file larger than SIZE",
        "--min-size=SIZE          don't transfer any file smaller than SIZE",
        "--partial                keep partially transferred files",
        "--partial-dir=DIR        put a partially transferred file into DIR",
        "--prune-empty-dirs, -m   prune empty directory chains from file-list",
        "--numeric-ids            don't map uid/gid values by user/group name",
        "--timeout=SECONDS        set I/O timeout in seconds",
        "--fuzzy, -y              find similar file for basis if no dest file",
        "--compress, -z           compress file data during the transfer",
        "--compress-level=NUM     explicitly set compression level",
        "--cvs-exclude, -C        auto-ignore files in the same way CVS does",
        "--filter=RULE, -f        add a file-filtering RULE",
        "-F                       same as --filter='dir-merge /.rsync-filter'",
        "--exclude=PATTERN        exclude files matching PATTERN",
        "--exclude-from=FILE      read exclude patterns from FILE",
        "--include=PATTERN        don't exclude files matching PATTERN",
        "--include-from=FILE      read include patterns from FILE",
        "--port=PORT              specify double-colon alternate port number",
        "--stats                  give some file-transfer stats",
        "--progress               show progress during transfer",
        "-P                       same as --partial --progress",
        "--itemize-changes, -i    output a change-summary for all updates",
        "--list-only              list the files instead of copying them",
        "--bwlimit=RATE           limit socket I/O bandwidth",
        "--write-batch=FILE       write a batched update to FILE (stub)",
        "--read-batch=FILE        read a batched update from FILE (stub)",
        "--protocol=NUM           force an older protocol version to be used",
        "--daemon                 run as an rsync daemon",
        "--config=FILE            specify alternate rsyncd.conf file",
        "--no-detach              do not detach from the parent (daemon mode)",
        "--version, -V            print the version + other info and exit",
        "--help, -h               show this help",
    ];
    for l in lines {
        // C rsync indents each option line with a single leading space.
        println!(" {l}");
    }
    print!(
        "\n\
         Use \"rsync --daemon --help\" to see the daemon-mode command-line options.\n\
         Please see the rsync(1) and rsyncd.conf(5) manpages for full documentation.\n\
         See https://github.com/lihongjie0209/rsync-rs for rsync-rs-specific notes.\n"
    );
}

// ── Stats output ──────────────────────────────────────────────────────────────

/// Emit the `Number of files: N (reg: A, dir: B, ...)` line that C rsync's
/// `output_itemized_counts()` produces.  Pass `[total, reg, dir, link, dev, spec]`.
fn itemized_counts(prefix: &str, counts: [i32; 5]) -> String {
    let total = counts[0];
    if total == 0 {
        return format!("{prefix}: 0\n");
    }
    let reg = total - (counts[1] + counts[2] + counts[3] + counts[4]);
    let labels = ["reg", "dir", "link", "dev", "special"];
    let vals = [reg, counts[1], counts[2], counts[3], counts[4]];
    let mut s = format!("{prefix}: {}", big_num(total as i64));
    let mut pre = " (";
    for j in 0..5 {
        if vals[j] != 0 {
            s.push_str(&format!("{pre}{}: {}", labels[j], big_num(vals[j] as i64)));
            pre = ", ";
        }
    }
    if pre == ", " {
        s.push(')');
    }
    s.push('\n');
    s
}

/// Print transfer statistics.  C rsync writes these via `rprintf(FINFO,...)`
/// which on the client goes to stdout, on the server gets multiplexed back
/// over the protocol pipe.  We don't multiplex back yet, so on server-mode
/// we emit to stderr (which the remote shell forwards to the client tty).
fn print_stats(stats: &Stats, elapsed_secs: f64, full: bool, dry_run: bool, am_server: bool) {
    let total_bytes = stats.total_written + stats.total_read;
    // Two-line writers: client → stdout, server → stderr (stdout is wire).
    macro_rules! out {
        ($($arg:tt)*) => {{
            if am_server { eprintln!($($arg)*); } else { println!($($arg)*); }
        }};
    }
    macro_rules! out_raw {
        ($($arg:tt)*) => {{
            if am_server { eprint!($($arg)*); } else { print!($($arg)*); }
        }};
    }

    if full {
        out_raw!("\n");
        out_raw!(
            "{}",
            itemized_counts(
                "Number of files",
                [stats.num_files, stats.num_dirs, stats.num_symlinks,
                 stats.num_devices, stats.num_specials],
            )
        );
        out_raw!(
            "{}",
            itemized_counts(
                "Number of created files",
                [stats.created_files, stats.created_dirs, stats.created_symlinks,
                 stats.created_devices, stats.created_specials],
            )
        );
        out_raw!(
            "{}",
            itemized_counts(
                "Number of deleted files",
                [stats.deleted_files, stats.deleted_dirs, stats.deleted_symlinks,
                 stats.deleted_devices, stats.deleted_specials],
            )
        );
        out!("Number of regular files transferred: {}", big_num(stats.xferred_files as i64));
        out!("Total file size: {} bytes", human_num(stats.total_size));
        out!("Total transferred file size: {} bytes", human_num(stats.total_transferred_size));
        out!("Literal data: {} bytes", human_num(stats.literal_data));
        out!("Matched data: {} bytes", human_num(stats.matched_data));
        out!("File list size: {}", human_num(stats.flist_size));
        if stats.flist_buildtime != 0 {
            out!("File list generation time: {} seconds",
                comma_dnum(stats.flist_buildtime as f64 / 1000.0, 3));
            out!("File list transfer time: {} seconds",
                comma_dnum(stats.flist_xfertime as f64 / 1000.0, 3));
        }
        out!("Total bytes sent: {}", human_num(stats.total_written));
        out!("Total bytes received: {}", human_num(stats.total_read));
    }

    out_raw!("\n");
    let rate = if elapsed_secs > 0.0 { total_bytes as f64 / elapsed_secs } else { 0.0 };
    out!(
        "sent {} bytes  received {} bytes  {} bytes/sec",
        big_num(stats.total_written),
        big_num(stats.total_read),
        comma_dnum(rate, 2)
    );
    let speedup =
        if total_bytes > 0 { stats.total_size as f64 / total_bytes as f64 } else { 0.0 };
    let suffix = if dry_run { " (DRY RUN)" } else { "" };
    out!(
        "total size is {}  speedup is {}{}",
        big_num(stats.total_size),
        comma_dnum(speedup, 2),
        suffix
    );
}

// ── Protocol handshake ────────────────────────────────────────────────────────

/// Perform the rsync version handshake.
///
/// Server reads first, then sends.  Client sends first, then reads.
/// Returns the negotiated (min) protocol version.
fn protocol_handshake<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    am_server: bool,
) -> Result<u32> {
    let local = PROTOCOL_VERSION;
    let remote: u32 = if am_server {
        let v = read_int(reader)? as u32;
        write_int(writer, local as i32)?;
        writer.flush().ok();
        v
    } else {
        write_int(writer, local as i32)?;
        writer.flush().ok();
        read_int(reader)? as u32
    };

    if remote < crate::protocol::constants::MIN_PROTOCOL_VERSION {
        anyhow::bail!(
            "remote protocol {} is too old (min {})",
            remote,
            crate::protocol::constants::MIN_PROTOCOL_VERSION
        );
    }
    if remote > crate::protocol::constants::MAX_PROTOCOL_VERSION {
        anyhow::bail!(
            "remote protocol {} is too new (max {})",
            remote,
            crate::protocol::constants::MAX_PROTOCOL_VERSION
        );
    }

    Ok(local.min(remote))
}

/// Exchange compatibility flags and checksum capability list (protocol >= 30).
///
/// Mirrors C rsync's `setup_protocol` compat section + `negotiate_the_strings`.
/// On the server side:
///   1. Write compat_flags as varint
///   2. If CF_VARINT_FLIST_FLAGS: write our checksum list as vstring, then read client's
///   3. Write checksum_seed as int32
///
/// Pick the highest-priority algorithm from `our_list` that also appears in
/// `peer_list`. Both are space-separated lowercase token vstrings.
fn negotiate(our_list: &str, peer_list: &str) -> Option<String> {
    let peer: std::collections::HashSet<&str> = peer_list.split_whitespace().collect();
    our_list
        .split_whitespace()
        .find(|tok| peer.contains(*tok))
        .map(|s| s.to_string())
}

/// Setup compat handshake for the server side.
///
/// The C peer sends its compress preference list (by default
/// "zstd lz4 zlibx zlib"). We advertise "zlib none" so the negotiation
/// picks "zlib" — supported via [`crate::delta::deflate_token`] — and
/// falls back to "none" only when explicitly chosen.
///
/// Returns `(do_varint, checksum_seed, compression_choice)` where the
/// compression choice is `"zlib"` or `"none"` (or `None` if the peer is
/// not requesting compression).
///
/// Returns (do_varint_flist_flags, checksum_seed).
fn setup_compat<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    opts: &Options,
    protocol: u32,
    do_compression: bool,
) -> Result<(bool, i32, Option<String>, u32)> {
    if protocol < 30 {
        return Ok((false, 0, None, 0));
    }

    // Build compat flags.  The C server sets CF_VARINT_FLIST_FLAGS when it sees
    // 'v' in client_info (which C rsync 3.2+ always sends).  We set it
    // unconditionally for protocol 31 since our flist always uses varint flags.
    let mut compat_flags: u32 = 0;
    if opts.recursive {
        compat_flags |= CF_INC_RECURSE;
    }
    compat_flags |= CF_VARINT_FLIST_FLAGS; // enables varint flist + checksum negotiation
    compat_flags |= CF_SAFE_FLIST;         // we honour the IO-error-endlist flag

    // Write compat_flags using varint (CF_VARINT_FLIST_FLAGS = 0x80 requires 2 bytes).
    write_varint(writer, compat_flags as i32)?;

    let do_varint = compat_flags & CF_VARINT_FLIST_FLAGS != 0;

    // ── Negotiation strings (rsync's negotiate_the_strings) ──────────────────
    // Mirror C compat.c: WRITE all our preference lists first, then READ peer's.
    // We don't actually compress on the wire, so when the peer wants
    // compression we propose only "none" — the C side accepts that and turns
    // do_compression off.
    if do_varint {
        write_vstring(writer, "md5")?;
        if do_compression {
            write_vstring(writer, "zlib none")?;
        }
    }

    // Flush now so the client can see our compat byte + checksum list,
    // write its own list, and then we can read it below.
    writer.flush().ok();
    if std::env::var_os("RSYNC_RS_DEBUG").is_some() {
        crate::rdebug!("[rsync-rs] wrote compat_flags=0x{:x} + checksum_list{}, flushed",
            compat_flags, if do_compression { " + compress_list" } else { "" });
    }

    // Read client's checksum list (sent after the client reads our compat flags).
    let client_list = if do_varint { read_vstring(reader).ok() } else { None };
    let client_compress = if do_varint && do_compression {
        read_vstring(reader).ok()
    } else { None };
    if std::env::var_os("RSYNC_RS_DEBUG").is_some() {
        crate::rdebug!("[rsync-rs] read client checksum list: {:?}", client_list);
        if do_compression {
            crate::rdebug!("[rsync-rs] read client compress list: {:?}", client_compress);
        }
    }

    // Write checksum seed (C rsync: `write_int(f_out, checksum_seed)` in setup_protocol).
    // Use time-based seed like C rsync: `time(NULL) ^ (getpid() << 6)`.
    let checksum_seed: i32 = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let p = std::process::id();
        (t as u32 ^ (p << 6)) as i32
    };
    write_int(writer, checksum_seed)?;
    writer.flush().ok();

    let compression_choice = if do_compression {
        client_compress
            .as_deref()
            .and_then(|peer| negotiate("zlib none", peer))
    } else {
        None
    };
    if std::env::var_os("RSYNC_RS_DEBUG").is_some() {
        crate::rdebug!("[rsync-rs] negotiated compression: {:?}", compression_choice);
    }

    Ok((do_varint, checksum_seed, compression_choice, compat_flags))
}

// ── Checksum type mapping ─────────────────────────────────────────────────────

fn strong_to_csum_type(
    ct: crate::checksum::strong::ChecksumType,
) -> crate::protocol::constants::CsumType {
    use crate::checksum::strong::ChecksumType as S;
    use crate::protocol::constants::CsumType as C;
    match ct {
        S::Md4Archaic => C::Md4Archaic,
        S::Md4Busted => C::Md4Busted,
        S::Md4Old => C::Md4Old,
        S::Md4 => C::Md4,
        S::Md5 => C::Md5,
        S::None => C::None,
    }
}

/// Client-side compat setup. Returns `(do_varint, checksum_seed, compression_choice, compat_flags)`.
fn setup_compat_client<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    protocol: u32,
    do_compression: bool,
) -> Result<(bool, i32, Option<String>, u32)> {
    if protocol < 30 {
        return Ok((false, 0, None, 0));
    }
    let dbg = std::env::var_os("RSYNC_RS_DEBUG").is_some();
    if dbg { crate::rdebug!("[client-compat] reading compat_flags varint..."); }
    // Read server compat_flags (written as varint by server).
    let compat_flags = read_varint(reader)? as u32;
    let do_varint = compat_flags & CF_VARINT_FLIST_FLAGS != 0;
    if dbg { crate::rdebug!("[client-compat] compat_flags=0x{:x} do_varint={}", compat_flags, do_varint); }

    let mut server_compress: Option<String> = None;
    if do_varint {
        // Server already wrote its checksum list; read it.
        let _server_list = read_vstring(reader).ok();
        server_compress = if do_compression { read_vstring(reader).ok() } else { None };
        // Write OUR checksum list before reading the seed (prevents deadlock).
        write_vstring(writer, "md5")?;
        if do_compression {
            write_vstring(writer, "zlib none")?;
        }
        writer.flush().ok();
    }

    // Read checksum seed written by server.
    let checksum_seed = read_int(reader)?;
    if dbg { crate::rdebug!("[client-compat] checksum_seed={}", checksum_seed); }

    let compression_choice = if do_compression {
        server_compress
            .as_deref()
            .and_then(|peer| negotiate("zlib none", peer))
    } else {
        None
    };
    if dbg { crate::rdebug!("[client-compat] negotiated compression: {:?}", compression_choice); }

    Ok((do_varint, checksum_seed, compression_choice, compat_flags))
}

// ── Server-mode dispatch ──────────────────────────────────────────────────────

/// Receive the filter-rule list sent by the remote peer and return a parsed
/// `FilterList`.
///
/// The C rsync client always sends a filter list (terminated by `write_int(0)`)
/// before the server sends or receives the file list.  We parse and return the
/// rules so the caller can apply them when building the file list.
fn recv_and_parse_filter_list<R: std::io::Read>(reader: &mut R) -> Result<crate::filter::FilterList> {
    let mut filter = crate::filter::FilterList::new();
    loop {
        let len = read_int(reader)?;
        if len == 0 {
            break;
        }
        let len = len as usize;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)?;
        if let Ok(s) = std::str::from_utf8(&buf) {
            filter.parse_rule(s);
        }
    }
    Ok(filter)
}

/// Receive and discard the filter-rule list (used in push/receiver paths where
/// we don't need to apply sender-side filtering).
fn recv_filter_list<R: std::io::Read>(reader: &mut R) -> Result<()> {
    loop {
        let len = read_int(reader)?;
        if len == 0 {
            break;
        }
        let len = len as usize;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)?;
    }
    Ok(())
}

/// Delete files in `dest_dir` that are not present in `flist`.
///
/// Builds a keep-set from every flist entry plus all their ancestor directories
/// (so that `sub/a.txt` in the flist keeps the `sub/` directory alive), then
/// walks `dest_dir` and removes anything not in that set.
///
/// Returns `(deleted_files, deleted_dirs, deleted_symlinks)`.
fn delete_extraneous_from_flist(
    flist: &crate::protocol::types::FileList,
    dest_dir: &std::path::Path,
    verbose: u8,
    dry_run: bool,
) -> (i32, i32, i32) {
    use std::collections::HashSet;
    use std::path::PathBuf;

    // Build keep-set: every flist path + all its ancestor directories.
    // Paths in flist use '/' as separator; normalize to OS separator for matching.
    let mut kept: HashSet<PathBuf> = HashSet::new();
    for fi in &flist.files {
        let raw = fi.path();
        if raw == "." || raw.is_empty() {
            continue;
        }
        let p = PathBuf::from(raw.replace('/', std::path::MAIN_SEPARATOR_STR));
        // Add all ancestors so no containing directory is deleted.
        let mut cur = p.as_path();
        loop {
            kept.insert(cur.to_path_buf());
            match cur.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => cur = parent,
                _ => break,
            }
        }
    }

    let mut del_files = 0i32;
    let mut del_dirs = 0i32;
    let mut del_symlinks = 0i32;

    fn walk(
        root: &std::path::Path,
        cur: &std::path::Path,
        kept: &HashSet<PathBuf>,
        verbose: u8,
        dry_run: bool,
        del_files: &mut i32,
        del_dirs: &mut i32,
        del_symlinks: &mut i32,
    ) {
        let Ok(entries) = std::fs::read_dir(cur) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = match path.strip_prefix(root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            };
            if kept.contains(&rel) {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    walk(root, &path, kept, verbose, dry_run, del_files, del_dirs, del_symlinks);
                }
                continue;
            }
            let ft = entry.file_type().ok();
            let is_dir = ft.as_ref().map(|t| t.is_dir()).unwrap_or(false);
            let is_symlink = ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false);
            if verbose > 0 {
                let rel_display = rel.to_string_lossy().replace('\\', "/");
                println!("deleting {rel_display}");
            }
            if !dry_run {
                if is_dir {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
            if is_dir {
                *del_dirs += 1;
            } else if is_symlink {
                *del_symlinks += 1;
            } else {
                *del_files += 1;
            }
        }
    }

    walk(dest_dir, dest_dir, &kept, verbose, dry_run, &mut del_files, &mut del_dirs, &mut del_symlinks);
    (del_files, del_dirs, del_symlinks)
}

/// Run as the server side of an rsync connection (invoked via remote shell).
pub fn run_server(opts: &Options) -> Result<Stats> {
    // C rsync sets stdin/stdout to non-blocking before spawning the server.
    // Our pipeline assumes blocking I/O (write_all, read_exact); restore that
    // by clearing O_NONBLOCK on both fds on Unix.
    #[cfg(unix)]
    unsafe {
        for fd in [0, 1] {
            let flags = libc::fcntl(fd, libc::F_GETFL, 0);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
            }
        }
    }

    let stdin = std::io::stdin();
    let raw_reader = stdin.lock();
    let stdout = std::io::stdout();
    let raw_writer = std::io::BufWriter::new(stdout.lock());
    run_server_io(opts, raw_reader, raw_writer)
}

/// Run the server protocol on an arbitrary reader/writer pair (used by
/// daemon mode).  Both halves must already be configured for blocking I/O.
pub fn run_server_io<R: std::io::Read, W: std::io::Write>(
    opts: &Options,
    raw_reader: R,
    raw_writer: W,
) -> Result<Stats> {
    let start = std::time::Instant::now();
    let mut reader = crate::io::multiplex::MplexReader::new(raw_reader);
    let mut writer = crate::io::multiplex::MplexWriter::new(raw_writer);

    // Step 1: Protocol version handshake.  In daemon mode (rsync://) the
    // version was already negotiated textually via "@RSYNCD: NN.S\n" so we
    // skip the 4-byte exchange here.
    let protocol = if opts.daemon {
        PROTOCOL_VERSION as u32
    } else {
        protocol_handshake(&mut reader, &mut writer, true)?
    };

    // Step 1b: Parse the bundled flags / argv that the client passed so we
    // know up-front whether compression negotiation will run.  Both server
    // branches (sender / receiver) use this; we compute it once here.
    let (mut server_flags, server_paths) =
        crate::options_server::parse_server_argv(&opts.args);
    server_flags.archive |= opts.archive;
    server_flags.recursive |= opts.recursive;
    server_flags.owner |= opts.owner;
    server_flags.group |= opts.group;
    server_flags.times |= opts.times;
    server_flags.compress |= opts.compress;
    server_flags.verbose += opts.verbose as u32;
    server_flags.expand_archive();
    if std::env::var_os("RSYNC_RS_DEBUG").is_some() {
        crate::rdebug!("[rsync-rs server] parsed args={:?} -> {}", opts.args, server_flags);
    }
    let do_compression = server_flags.compress;

    // Step 2: Compat flags + checksum/compress negotiation + checksum seed.
    // This mirrors C rsync's setup_protocol() completion:
    //   server writes compat_flags (varint) + checksum list (vstring) + checksum_seed (int)
    //   client reads compat_flags, writes its checksum list, reads server's list + seed.
    let (do_varint_flist, checksum_seed, compression_choice, server_compat_flags) =
        if protocol >= 30 {
            setup_compat(&mut reader, &mut writer, opts, protocol, do_compression)?
        } else {
            (false, 0, None, 0u32)
        };
    let _ = do_varint_flist; // stored for future flist encoding selection
    let use_zlib = matches!(compression_choice.as_deref(), Some("zlib"));

    let csum_ct = crate::checksum::strong::ChecksumType::for_protocol(protocol, false);
    let rsync_ct = strong_to_csum_type(csum_ct);
    // checksum_len > 0 means every file entry in the list carries a whole-file
    // checksum.  In server mode the flag arrives as the bundled short 'c' inside
    // server_flags; opts.checksum may be false even when the client requested it.
    let use_checksum = opts.checksum || server_flags.checksum;
    let checksum_len = if use_checksum { csum_ct.digest_len() } else { 0 };

    // Step 3: Enable multiplexed I/O on both directions.
    // C rsync: io_start_multiplex_out() for protocol >= 23
    //          io_start_multiplex_in()  for protocol >= 30 (need_messages_from_generator=1)
    // The client (receiver) will also enable mux out (for generator messages) and send
    // the filter list as mux-framed data, so we must enable mux in before reading it.
    writer.enable();
    if protocol >= 30 {
        reader.enable();
    }
    crate::rdebug!("[rsync-rs] mux enabled (in={}, out=true), opts.sender={}", reader.is_enabled(), opts.sender);

    if opts.sender {
        // Server IS the sender (client is pulling files FROM us).
        unsafe { log_mod::set_who("sender") };

        // Step 4: Receive and parse filter rules from client (terminated by int(0)).
        crate::rdebug!("[rsync-rs] waiting for filter list...");
        let client_filter = recv_and_parse_filter_list(&mut reader).context("recv_filter_list")?;
        crate::rdebug!("[rsync-rs] filter list received");

        // Step 5: Resolve source paths.
        // args[0] is the base dir to chdir into (C rsync: start_server uses argv[0]),
        // args[1..] are the actual source paths. server_flags / server_paths
        // were computed up-front (see Step 1b) so we know preserve booleans
        // before mux/negotiation.
        let path_args = &server_paths;
        let preserve = server_flags.to_preserve();
        let mut recursive = server_flags.recursive;
        if opts.recursive || opts.archive {
            recursive = true;
        }
        let (base_dir_arg, src_paths): (&str, Vec<&str>) = if path_args.len() > 1 {
            (path_args[0], path_args[1..].to_vec())
        } else if !path_args.is_empty() {
            (path_args[0], vec![path_args[0]])
        } else {
            (".", vec!["."])
        };

        // Chdir into base_dir (mirrors C rsync's start_server argv[0] cd).
        if let Err(e) = std::env::set_current_dir(base_dir_arg) {
            crate::rdebug!("[rsync-rs] chdir {:?} failed: {}", base_dir_arg, e);
        }
        crate::rdebug!("[rsync-rs] src_paths: {:?}", src_paths);

        // Step 6: Build file list from local source paths.
        // Files are sent with names relative to the source directory root.
        let mut flist = crate::protocol::types::FileList::new();
        for src in &src_paths {
            let path = std::path::Path::new(src);
            if path.is_dir() {
                // Add the root '.' directory entry first so C rsync runs
                // delete_in_dir() for the root (FLAG_CONTENT_DIR via XMIT_TOP_DIR).
                if let Ok(meta) = path.metadata() {
                    let mut root_fi = file_info_from_meta(".", None, &meta);
                    root_fi.flags |= crate::protocol::constants::FLAG_TOP_DIR;
                    flist.files.push(root_fi);
                }
                walk_source_dir(path, "", recursive, &mut flist, &client_filter);
            } else if let Ok(meta) = std::fs::metadata(path) {
                let name =
                    path.file_name().and_then(|n| n.to_str()).unwrap_or(src).to_string();
                flist.files.push(file_info_from_meta(&name, None, &meta));
            }
        }
        crate::flist::flist_sort(&mut flist);
        crate::rdebug!("[rsync-rs] mark_hardlinks: hard_links={}", server_flags.hard_links);
        if std::env::var_os("RSYNC_RS_DEBUG").is_some() {
            for (i, fi) in flist.files.iter().enumerate() {
                crate::rdebug!("[rsync-rs]   pre-mark[{}] {:?} ino={} dev={}", i, fi.path(), fi.ino, fi.dev);
            }
        }
        mark_hardlinks(&mut flist, server_flags.hard_links);
        crate::rdebug!("[rsync-rs] flist has {} files", flist.files.len());
        for (i, fi) in flist.files.iter().enumerate() {
            crate::rdebug!("[rsync-rs]   sorted[{}] = {:?} flags=0x{:x} hlink_ndx={}", i, fi.path(), fi.flags, fi.hard_link_first_ndx);
        }

        // Step 7: Send file list (through multiplexed output).
        crate::rdebug!("[rsync-rs] sending file list...");
        crate::flist::send_file_list_ex(&mut writer, &flist, protocol, checksum_len, 0, preserve, server_compat_flags)
            .context("send_file_list")?;
        crate::rdebug!("[rsync-rs] flushing after send_file_list");
        writer.flush().ok();

        // Step 8: Run sender pipeline: reads raw checksums from client, writes mux tokens.
        // base_dir is the directory we look up files in. If src_paths[0] is a file
        // (single-file mode), use its parent directory; otherwise the path itself.
        let base_dir: std::path::PathBuf = match src_paths.first().copied() {
            Some(p) => {
                let path = std::path::Path::new(p);
                if path.is_file() {
                    path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()
                } else {
                    path.to_path_buf()
                }
            }
            None => std::path::PathBuf::from("."),
        };
        crate::rdebug!("[rsync-rs] sender base_dir={:?}", base_dir);
        crate::rdebug!("[rsync-rs] entering sender loop, flist.len={}", flist.files.len());
        let final_stats: Stats = {
            let mut sender = pipeline::Sender::new(&mut reader, &mut writer).with_compression(use_zlib);
            sender.run(&flist, &base_dir, rsync_ct, protocol, checksum_seed).context("sender run")?;
            crate::rdebug!("[rsync-rs] sender loop done");
            sender.stats.clone()
        };

        // Step 9: handle_stats — server-sender writes 3 varlong30 stats (5 for proto >= 29).
        crate::io::varint::write_varlong(&mut writer, final_stats.total_read, 3)?;
        crate::io::varint::write_varlong(&mut writer, final_stats.total_written, 3)?;
        crate::io::varint::write_varlong(&mut writer, final_stats.total_size, 3)?;
        if protocol >= 29 {
            crate::io::varint::write_varlong(&mut writer, 0, 3)?; // flist_buildtime
            crate::io::varint::write_varlong(&mut writer, 0, 3)?; // flist_xfertime
        }
        writer.flush().ok();
        crate::rdebug!("[rsync-rs] stats sent");

        // Step 10: read_final_goodbye — read NDX_DONE from client, echo back for proto >= 31.
        if protocol >= 29 {
            let i = crate::io::varint::read_ndx(&mut reader)?;
            crate::rdebug!("[rsync-rs] final NDX={}", i);
            if protocol >= 31 && i == crate::protocol::constants::NDX_DONE {
                crate::io::varint::write_ndx(&mut writer, crate::protocol::constants::NDX_DONE)?;
                writer.flush().ok();
                let i2 = crate::io::varint::read_ndx(&mut reader)?;
                crate::rdebug!("[rsync-rs] final NDX (2nd)={}", i2);
            }
        }
        Ok(final_stats)
    } else {
        // Server IS the receiver (client is pushing files TO us).
        unsafe { log_mod::set_who("receiver") };
        crate::rdebug!("[rsync-rs] receiver: starting recv_file_list");

        // When --delete is active, receiver_wants_list=1 on both sides:
        // the client (sender) sends a filter list before the flist and we
        // must drain it here.  Without --delete the client sends nothing,
        // so we must not block on a read.
        let server_flags_delete = server_flags.delete;
        if server_flags_delete {
            crate::rdebug!("[rsync-rs] receiver: draining filter list (delete mode)");
            recv_filter_list(&mut reader).context("recv_filter_list (push+delete)")?;
        }

        // Server-receiver: server_flags / preserve already computed (Step 1b).
        let preserve = server_flags.to_preserve();

        // Step 5: Receive file list from client.
        let flist = crate::flist::recv_file_list_ex(&mut reader, protocol, checksum_len, preserve.uid, preserve.gid, server_compat_flags)
            .context("recv_file_list")?;
        crate::rdebug!("[rsync-rs] receiver: flist done ({} entries)", flist.files.len());
        // (The checksum seed was already exchanged during setup_compat; the C
        // client sender does NOT send a second seed after the flist.)

        let dest_dir =
            opts.args.last().map(std::path::Path::new).unwrap_or(std::path::Path::new("."));

        // --delete: remove extraneous files from dest before transferring.
        let (del_files, del_dirs, del_symlinks) = if server_flags_delete && dest_dir.is_dir() {
            delete_extraneous_from_flist(&flist, dest_dir, opts.verbose, opts.dry_run)
        } else {
            (0, 0, 0)
        };

        let mut stats = pipeline::receiver::run_server_receiver(
            &mut reader, &mut writer, &flist, dest_dir, rsync_ct, protocol, checksum_seed,
            use_zlib, opts.inplace, opts.itemize_changes, use_checksum,
        )
        .context("server-receiver run")?;
        stats.deleted_files = del_files;
        stats.deleted_dirs = del_dirs;
        stats.deleted_symlinks = del_symlinks;
        let _ = checksum_seed;
        if opts.stats || opts.verbose > 0 {
            print_stats(&stats, start.elapsed().as_secs_f64(), opts.stats, opts.dry_run, true);
        }
        Ok(stats)
    }
}

// ── Client-mode dispatch ──────────────────────────────────────────────────────

/// `--list-only` for purely local sources: walk and print `ls -l`-style lines.
fn list_only_local(args: &[String]) -> Result<Stats> {
    use std::time::UNIX_EPOCH;

    fn fmt_mode(mode: u32, is_dir: bool, is_link: bool) -> String {
        let t = if is_link { 'l' } else if is_dir { 'd' } else { '-' };
        let p = |bits: u32, ch: char| if mode & bits != 0 { ch } else { '-' };
        format!(
            "{t}{}{}{}{}{}{}{}{}{}",
            p(0o400, 'r'), p(0o200, 'w'), p(0o100, 'x'),
            p(0o040, 'r'), p(0o020, 'w'), p(0o010, 'x'),
            p(0o004, 'r'), p(0o002, 'w'), p(0o001, 'x'),
        )
    }

    fn fmt_ts(secs: i64) -> String {
        // Inline UTC->local Y/M/D H:M:S without pulling chrono in.
        // Fall back to a fixed string on bad timestamps.
        use std::time::{SystemTime, Duration};
        let st = SystemTime::UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64);
        // Use the "%Y/%m/%d %H:%M:%S" format via humantime if present, else
        // a hand-rolled gmtime. We hand-roll to keep deps minimal.
        let _ = st;
        let mut s = secs.max(0) as u64;
        let secs_of_day = (s % 86400) as u32;
        let days = s / 86400;
        let _ = s;
        // Civil-from-days algorithm (Howard Hinnant).
        let z = days as i64 + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u64;
        let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365*yoe + yoe/4 - yoe/100);
        let mp = (5*doy + 2) / 153;
        let d = doy - (153*mp + 2)/5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let yr = y + (if m <= 2 { 1 } else { 0 });
        let h = secs_of_day / 3600;
        let mi = (secs_of_day / 60) % 60;
        let se = secs_of_day % 60;
        format!("{yr:04}/{m:02}/{d:02} {h:02}:{mi:02}:{se:02}")
    }

    fn print_one(rel: &str, meta: &std::fs::Metadata) {
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode: u32 = if meta.permissions().readonly() { 0o444 } else { 0o644 };
        let modestr = fmt_mode(mode, meta.is_dir(), meta.file_type().is_symlink());
        let size = meta.len() as i64;
        let mtime = meta.modified().ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let ts = fmt_ts(mtime);
        println!("{modestr} {:>14} {ts} {rel}", big_num(size));
    }

    let mut stats = Stats::default();
    for arg in args {
        let path = std::path::PathBuf::from(arg);
        let meta = std::fs::symlink_metadata(&path)
            .with_context(|| format!("stat {arg}"))?;
        let base = path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| arg.clone());
        if meta.is_dir() {
            print_one(&format!("{base}/"), &meta);
            stats.num_dirs += 1;
            list_dir_recursive(&path, &base, &mut stats)?;
        } else {
            print_one(&base, &meta);
            if meta.file_type().is_symlink() { stats.num_symlinks += 1; }
            else { stats.num_files += 1; stats.total_size += meta.len() as i64; }
        }
    }
    Ok(stats)
}

fn list_dir_recursive(root: &std::path::Path, prefix: &str, stats: &mut Stats) -> Result<()> {
    use std::time::UNIX_EPOCH;
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .with_context(|| format!("read_dir {root:?}"))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let m = e.metadata().with_context(|| format!("stat {p:?}"))?;
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}/{name}");
        // Print this entry inline (avoid re-implementing print_one here).
        let mode = {
            #[cfg(unix)]
            { use std::os::unix::fs::PermissionsExt; m.permissions().mode() }
            #[cfg(not(unix))]
            { if m.permissions().readonly() { 0o444u32 } else { 0o644u32 } }
        };
        let t = if m.file_type().is_symlink() { 'l' } else if m.is_dir() { 'd' } else { '-' };
        let perm = |bits: u32, ch: char| if mode & bits != 0 { ch } else { '-' };
        let modestr = format!(
            "{t}{}{}{}{}{}{}{}{}{}",
            perm(0o400, 'r'), perm(0o200, 'w'), perm(0o100, 'x'),
            perm(0o040, 'r'), perm(0o020, 'w'), perm(0o010, 'x'),
            perm(0o004, 'r'), perm(0o002, 'w'), perm(0o001, 'x'),
        );
        let mtime = m.modified().ok()
            .and_then(|x| x.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let ts = {
            let mut s = mtime.max(0) as u64;
            let secs_of_day = (s % 86400) as u32;
            let days = s / 86400; let _ = s;
            let z = days as i64 + 719468;
            let era = if z >= 0 { z } else { z - 146096 } / 146097;
            let doe = (z - era * 146097) as u64;
            let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
            let y = yoe as i64 + era * 400;
            let doy = doe - (365*yoe + yoe/4 - yoe/100);
            let mp = (5*doy + 2) / 153;
            let d = doy - (153*mp + 2)/5 + 1;
            let mo = if mp < 10 { mp + 3 } else { mp - 9 };
            let yr = y + (if mo <= 2 { 1 } else { 0 });
            let h = secs_of_day / 3600;
            let mi = (secs_of_day / 60) % 60;
            let se = secs_of_day % 60;
            format!("{yr:04}/{mo:02}/{d:02} {h:02}:{mi:02}:{se:02}")
        };
        let trail = if m.is_dir() { "/" } else { "" };
        println!("{modestr} {:>14} {ts} {rel}{trail}", big_num(m.len() as i64));
        if m.is_dir() {
            stats.num_dirs += 1;
            list_dir_recursive(&p, &rel, stats)?;
        } else if m.file_type().is_symlink() {
            stats.num_symlinks += 1;
        } else {
            stats.num_files += 1;
            stats.total_size += m.len() as i64;
        }
    }
    Ok(())
}

fn run_client(opts: &Options) -> Result<Stats> {
    let start = std::time::Instant::now();

    // --list-only with a local-only source list: walk and print like `ls -l`.
    // (Remote list-only would require the protocol; we still defer that.)
    if opts.list_only && !opts.args.is_empty() {
        let any_remote = opts.args.iter().any(|a|
            Options::parse_remote_src(a).is_some()
            || Options::parse_remote_dst(a).is_some()
        );
        if !any_remote {
            let stats = list_only_local(&opts.args)?;
            return Ok(stats);
        }
    }

    let (sources, dest) = opts.parse_paths().context("parsing source/destination")?;

    // --read-batch: apply a previously-written batch file to the destination.
    if let Some(batch_path) = opts.read_batch.as_ref().map(|s| s.clone()) {
        batch::run_read_batch(opts, &batch_path, &dest)
            .context("read-batch")?;
        return Ok(Default::default());
    }

    // Determine transfer direction.
    let remote_dst = Options::parse_remote_dst(&dest);
    let remote_src = sources.iter().find_map(|s| Options::parse_remote_src(s));

    // is_push = true  → we send files (sources are local, dest is remote)
    // is_push = false → we receive files (sources are remote, dest is local)
    let (remote_spec, remote_path, is_push) = if let Some(spec) = remote_dst {
        let path = spec.path.clone();
        (spec, path, true)
    } else if let Some(spec) = remote_src {
        let path = spec.path.clone();
        (spec, path, false)
    } else {
        // Both sides are local: walk the source tree directly, no protocol dance.
        // This is faster, simpler, and stays fully cross-platform.
        if opts.verbose >= 1 {
            println!("sending incremental file list");
        }
        let report = pipeline::run_local(opts, &sources, &dest)
            .context("local transfer")?;

        // --write-batch: capture the flist + contents to a batch file.
        if let Some(batch_path) = opts.write_batch.as_ref().map(|s| s.clone()) {
            let filter = crate::filter::FilterList::from_options(opts).unwrap_or_default();
            let recursive = opts.recursive || opts.archive;
            let mut flist = crate::protocol::types::FileList::new();
            for src in &sources {
                let p = std::path::Path::new(src);
                walk_source_dir(p, "", recursive, &mut flist, &filter);
            }
            crate::flist::flist_sort(&mut flist);
            batch::run_write_batch(opts, &flist, &sources, &batch_path)
                .context("write-batch")?;
        }

        if opts.stats || opts.verbose > 0 {
            print_stats(&report.stats, start.elapsed().as_secs_f64(), opts.stats, opts.dry_run, false);
        }
        return Ok(report.stats);
    };

    if remote_spec.is_daemon {
        return run_client_daemon(opts, &remote_spec, &remote_path, is_push, start);
    }

    // Build the SSH transport.
    let ssh_cmd = opts.rsh.as_deref().unwrap_or("ssh");
    let rsync_path = opts.rsync_path.as_deref().unwrap_or("rsync");
    let mut server_args = opts.server_args();
    if !is_push {
        // Server will be the sender.
        server_args.push("--sender".into());
    }

    let ssh = transport::SshTransport::connect(
        ssh_cmd,
        &[],
        &remote_spec.host,
        remote_spec.user.as_deref(),
        rsync_path,
        &server_args,
        &remote_path,
    )
    .context("ssh connect")?;

    let (mut stdin_pipe, mut stdout_pipe, ssh_child) = ssh.split();

    crate::rdebug!("[rsync-rs client] starting protocol handshake...");
    let protocol = protocol_handshake(&mut stdout_pipe, &mut stdin_pipe, false)?;
    crate::rdebug!("[rsync-rs client] protocol={}", protocol);

    let stats = run_client_protocol(
        opts, &sources, &dest, is_push,
        stdout_pipe, stdin_pipe, protocol,
    )?;
    let _ = ssh_child.wait();
    if opts.stats || opts.verbose > 0 {
        print_stats(&stats, start.elapsed().as_secs_f64(), opts.stats, opts.dry_run, false);
    }
    Ok(stats)
}

/// Daemon-mode client: open TCP, walk the @RSYNCD greeting, then run the
/// normal client protocol pipeline.  The textual handshake also yields the
/// negotiated protocol version, so we skip the binary 4-byte exchange that
/// SSH-mode does.
fn run_client_daemon(
    opts: &Options,
    remote_spec: &crate::options::RemoteSpec,
    remote_path: &str,
    is_push: bool,
    start: std::time::Instant,
) -> Result<Stats> {
    // Split "MOD/sub/path" into ("MOD", "sub/path").  The daemon switches
    // its cwd to the module dir, so the sub-path is what we actually want
    // to send/receive.
    let (module, sub_path) = match remote_path.split_once('/') {
        Some((m, p)) => (m.to_string(), p.to_string()),
        None => (remote_path.to_string(), String::new()),
    };
    if module.is_empty() {
        anyhow::bail!("rsync:// URL is missing a module name (got '/{remote_path}')");
    }

    let port = remote_spec.port.unwrap_or(873);

    // Build the argv we'll send to the daemon.  Same shape as SSH server
    // args but with the path slot pointing at the module sub-path (or "."
    // if the URL was just /MOD/).
    let mut server_argv = opts.server_args();
    if !is_push {
        server_argv.push("--sender".into());
    }
    // Append the path operands.  C clients send: ".", then the path(s).
    // Match the C client byte-for-byte: the user-provided path arg from the
    // rsync:// URL is preserved verbatim, including a trailing slash if the
    // URL had one.  The daemon-side server uses this to distinguish
    // "module root" (with slash) from "module name as a file" (no slash).
    server_argv.push(".".into());
    let path_arg = if sub_path.is_empty() {
        // URL was rsync://host/MOD/  -- send "MOD/"
        format!("{module}/")
    } else if remote_path.ends_with('/') {
        // URL ended with a slash on a sub-path; preserve it.
        format!("{module}/{sub_path}")
    } else {
        format!("{module}/{}", sub_path.trim_end_matches('/'))
    };
    server_argv.push(path_arg);

    let dc = transport::DaemonClient::connect(&remote_spec.host, port, &module, &server_argv)
        .context("rsync daemon connect")?;
    let protocol = dc.protocol;
    crate::rdebug!("[rsync-rs daemon-client] negotiated protocol={protocol}");

    let stats = run_client_protocol(
        opts,
        &if is_push {
            // Sources stay as the local args from the CLI.
            // We pull them out of opts.parse_paths() result via a re-parse.
            let (sources, _) = opts.parse_paths().unwrap_or_default();
            sources
        } else {
            // For pull, sources is the rsync:// URL; the protocol pipeline
            // needs to know "the receiver writes here" but doesn't reach
            // for `sources` itself.  Pass an empty slice.
            Vec::new()
        },
        &if is_push {
            // For a push, dest is unused inside the protocol body; pass empty.
            String::new()
        } else {
            // Receiver writes into the local destination from the CLI.
            opts.parse_paths().map(|(_, d)| d).unwrap_or_default()
        },
        is_push,
        dc.reader,
        dc.writer,
        protocol,
    )?;
    if opts.stats || opts.verbose > 0 {
        print_stats(&stats, start.elapsed().as_secs_f64(), opts.stats, opts.dry_run, false);
    }
    Ok(stats)
}

/// Run the post-handshake client protocol body over an already-opened pair
/// of byte streams.  Used by both the SSH transport (where `protocol` came
/// from `protocol_handshake`) and the rsync:// daemon transport (where it
/// came from the textual `@RSYNCD:` greeting).
fn run_client_protocol<R: std::io::Read, W: std::io::Write>(
    opts: &Options,
    sources: &[String],
    dest: &str,
    is_push: bool,
    mut stdout_pipe: R,
    mut stdin_pipe: W,
    protocol: u32,
) -> Result<Stats> {
    let (_do_varint, checksum_seed, compression_choice, compat_flags) = if protocol >= 30 {
        crate::rdebug!("[rsync-rs client] starting setup_compat_client...");
        let r = setup_compat_client(&mut stdout_pipe, &mut stdin_pipe, protocol, opts.compress)?;
        crate::rdebug!("[rsync-rs client] setup_compat_client done, seed={}, compat=0x{:x}", r.1, r.3);
        r
    } else {
        (false, 0, None, 0u32)
    };
    let use_zlib = matches!(compression_choice.as_deref(), Some("zlib"));

    let csum_ct = crate::checksum::strong::ChecksumType::for_protocol(protocol, false);
    let rsync_ct = strong_to_csum_type(csum_ct);
    let checksum_len = if opts.checksum { csum_ct.digest_len() } else { 0 };

    // Wrap I/O in multiplexers (matches C's `io_start_multiplex_*` calls).
    // For protocol >= 30, both directions are multiplexed for client-sender
    // and (with `need_messages_from_generator=1` for proto>=31) for
    // client-receiver too.
    let mut reader = crate::io::multiplex::MplexReader::new(stdout_pipe);
    let mut writer = crate::io::multiplex::MplexWriter::new(std::io::BufWriter::new(stdin_pipe));
    if protocol >= 30 {
        reader.enable();
        writer.enable();
    }

    let preserve = crate::flist::send::Preserve {
        uid: opts.owner || opts.archive,
        gid: opts.group || opts.archive,
        times: opts.times || opts.archive,
        devices: false,
    };

    let stats = if is_push {
        // Build flist from local sources by walking the tree.
        let local_filter = crate::filter::FilterList::from_options(&opts)
            .unwrap_or_default();
        let mut flist = crate::protocol::types::FileList::new();
        for src in sources {
            let p = std::path::Path::new(src);
            let recursive = opts.recursive;
            if p.is_dir() {
                // Add the root '.' directory entry so C rsync runs
                // delete_in_dir() for the root when --delete is active.
                if let Ok(meta) = p.metadata() {
                    let mut root_fi = file_info_from_meta(".", None, &meta);
                    root_fi.flags |= crate::protocol::constants::FLAG_TOP_DIR;
                    flist.files.push(root_fi);
                }
                walk_source_dir(p, "", recursive, &mut flist, &local_filter);
            } else if let Ok(meta) = p.symlink_metadata() {
                let name =
                    p.file_name().and_then(|n| n.to_str()).unwrap_or(src).to_string();
                let mut fi = file_info_from_meta(&name, None, &meta);
                if meta.file_type().is_symlink() {
                    if let Ok(t) = std::fs::read_link(p) {
                        fi.link_target = Some(t.to_string_lossy().into_owned());
                    }
                }
                flist.files.push(fi);
            }
        }
        crate::flist::flist_sort(&mut flist);
        mark_hardlinks(&mut flist, opts.hard_links);

        if opts.verbose >= 1 {
            println!("sending incremental file list");
        }

        // When --delete is set, receiver_wants_list=1 and the server expects
        // a filter list before the file list.  Send an empty one (terminator=0).
        if opts.delete || opts.delete_before || opts.delete_during || opts.delete_after {
            write_int(&mut writer, 0)?;
            writer.flush().ok();
        }

        crate::flist::send::send_file_list_ex(&mut writer, &flist, protocol, checksum_len, 0, preserve, compat_flags)
            .context("send_file_list")?;
        writer.flush().ok();

        // Find base directory for sender pipeline.
        let base_dir: std::path::PathBuf = if let Some(s) = sources.first() {
            let p = std::path::Path::new(s);
            if p.is_dir() {
                p.to_path_buf()
            } else {
                p.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()
            }
        } else {
            std::path::PathBuf::from(".")
        };

        let final_stats: Stats = {
            let mut sender = pipeline::Sender::new(&mut reader, &mut writer).with_compression(use_zlib);
            sender.run(&flist, &base_dir, rsync_ct, protocol, checksum_seed)
                .context("client sender run")?;
            sender.stats.clone()
        };

        if protocol >= 29 {
            let _ = if protocol >= 30 { read_int_or_ndx(&mut reader, protocol).ok() } else { None };
            if protocol >= 31 {
                if protocol >= 30 {
                    crate::io::varint::write_ndx(&mut writer, crate::protocol::constants::NDX_DONE)?;
                } else {
                    write_int(&mut writer, crate::protocol::constants::NDX_DONE)?;
                }
                writer.flush().ok();
                let _ = read_int_or_ndx(&mut reader, protocol).ok();
            }
        }
        final_stats
    } else {
        // Client-receiver (pull): send our filter rules to the server so it
        // can apply them when building the file list.
        crate::rdebug!("[rsync-rs client] sending filter list...");
        {
            let filter = crate::filter::FilterList::from_options(&opts).unwrap_or_default();
            for rule in &filter.rules {
                // C rsync format: write_int(len) then "- pattern" or "+ pattern"
                let is_include = rule.rflags & crate::filter::FILTRULE_INCLUDE != 0;
                let prefix = if is_include { "+ " } else { "- " };
                let rule_str = format!("{}{}", prefix, rule.pattern);
                write_int(&mut writer, rule_str.len() as i32)?;
                writer.write_all(rule_str.as_bytes())?;
            }
        }
        write_int(&mut writer, 0)?;
        writer.flush().ok();

        if opts.verbose >= 1 {
            println!("receiving incremental file list");
        }
        let flist = crate::flist::recv_file_list_ex(&mut reader, protocol, checksum_len, preserve.uid, preserve.gid, compat_flags)
            .context("recv_file_list")?;
        crate::rdebug!("[rsync-rs client] received flist with {} entries", flist.files.len());

        let dest_path = std::path::Path::new(dest);
        let _ = std::fs::create_dir_all(dest_path);

        // --delete-before: remove extraneous files from dest after receiving
        // the flist but before transferring any data.
        let (del_files, del_dirs, del_symlinks) =
            if (opts.delete || opts.delete_before || opts.delete_during || opts.delete_after)
                && dest_path.is_dir()
            {
                delete_extraneous_from_flist(&flist, dest_path, opts.verbose, opts.dry_run)
            } else {
                (0, 0, 0)
            };

        let mut stats = pipeline::receiver::run_server_receiver(
            &mut reader, &mut writer, &flist, dest_path, rsync_ct, protocol, checksum_seed,
            use_zlib, opts.inplace, opts.itemize_changes, opts.checksum,
        ).context("client-receiver run")?;
        stats.deleted_files = del_files;
        stats.deleted_dirs = del_dirs;
        stats.deleted_symlinks = del_symlinks;

        if protocol >= 29 {
            let total_written = crate::io::varint::read_varlong(&mut reader, 3).unwrap_or(0);
            let total_read = crate::io::varint::read_varlong(&mut reader, 3).unwrap_or(0);
            let total_size = crate::io::varint::read_varlong(&mut reader, 3).unwrap_or(0);
            let _ = crate::io::varint::read_varlong(&mut reader, 3).ok();
            let _ = crate::io::varint::read_varlong(&mut reader, 3).ok();
            stats.total_written = total_written;
            stats.total_read = total_read;
            stats.total_size = total_size;
        }
        stats
    };

    drop(writer);
    drop(reader);
    Ok(stats)
}

/// Read either NDX (protocol 30+) or int (legacy) — best-effort.
fn read_int_or_ndx<R: std::io::Read>(r: &mut R, protocol: u32) -> Result<i32> {
    if protocol >= 30 {
        crate::io::varint::read_ndx(r)
    } else {
        Ok(read_int(r)?)
    }
}

// ── File-info helpers ─────────────────────────────────────────────────────────

/// After flist_sort, group regular files by (dev, ino) and mark hardlink
/// leader/follower relationships.  Only runs on Unix and only when `hard_links`
/// is true.
///
/// Leaders get `FLAG_HLINKED | FLAG_HLINK_FIRST`; followers get `FLAG_HLINKED`
/// and `hard_link_first_ndx` set to the wire index of their leader
/// (`flist.ndx_start + leader_wire_pos`).
fn mark_hardlinks(flist: &mut crate::protocol::types::FileList, hard_links: bool) {
    if !hard_links {
        return;
    }
    #[cfg(unix)]
    {
        use std::collections::HashMap;
        use crate::protocol::constants::{FLAG_HLINKED, FLAG_HLINK_FIRST};

        // Group wire positions by (dev, ino).  We only care about regular files.
        let mut groups: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
        for wire_pos in 0..flist.sorted.len() {
            let idx = flist.sorted[wire_pos];
            let fi = &flist.files[idx];
            if !fi.is_regular() || fi.ino == 0 {
                continue;
            }
            groups.entry((fi.dev, fi.ino)).or_default().push(wire_pos);
        }

        // For groups with 2+ members, assign hardlink flags.
        for (_, mut positions) in groups {
            if positions.len() < 2 {
                continue;
            }
            positions.sort_unstable();
            let leader_wire_pos = positions[0] as i32;
            for (i, wire_pos) in positions.iter().enumerate() {
                let idx = flist.sorted[*wire_pos];
                if i == 0 {
                    flist.files[idx].flags |= FLAG_HLINKED | FLAG_HLINK_FIRST;
                } else {
                    flist.files[idx].flags |= FLAG_HLINKED;
                    flist.files[idx].hard_link_first_ndx =
                        flist.ndx_start + leader_wire_pos;
                }
            }
        }
    }
}

/// Walk a source directory adding entries to the flist.
///
/// `prefix` is the relative path prefix to prepend (empty for the root call).
/// When `recursive` is false, sub-directory contents are skipped but the
/// directory entry itself is still added (matching C rsync's `-d` behaviour
/// is out-of-scope here; for non-recursive we just include the listed files).
fn walk_source_dir(
    dir: &std::path::Path,
    prefix: &str,
    recursive: bool,
    flist: &mut crate::protocol::types::FileList,
    filter: &crate::filter::FilterList,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        // Use symlink_metadata so we capture the link itself, not the target.
        let meta = match entry.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let raw_name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            raw_name.clone()
        } else {
            format!("{prefix}/{raw_name}")
        };
        let (dirname, name) = split_rel(&rel);

        let ft = meta.file_type();
        // Apply exclude filter using full relative path (basename-only patterns
        // automatically match against the last component inside is_excluded).
        if filter.is_excluded(&rel, ft.is_dir()) {
            continue;
        }
        if ft.is_symlink() {
            let mut fi = file_info_from_meta(&name, dirname.as_deref(), &meta);
            if let Ok(target) = std::fs::read_link(entry.path()) {
                fi.link_target = Some(target.to_string_lossy().into_owned());
            }
            flist.files.push(fi);
        } else if ft.is_dir() {
            flist.files.push(file_info_from_meta(&name, dirname.as_deref(), &meta));
            if recursive {
                walk_source_dir(&entry.path(), &rel, true, flist, filter);
            }
        } else {
            flist.files.push(file_info_from_meta(&name, dirname.as_deref(), &meta));
        }
    }
}

/// Split a forward-slash relative path into `(dirname, basename)`.
fn split_rel(rel: &str) -> (Option<String>, String) {
    match rel.rfind('/') {
        Some(p) => (Some(rel[..p].to_string()), rel[p + 1..].to_string()),
        None => (None, rel.to_string()),
    }
}

#[allow(dead_code)]
fn file_info_from_path(path: &str, meta: &std::fs::Metadata) -> crate::protocol::types::FileInfo {
    let p = std::path::Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or(path).to_string();
    let dirname = p
        .parent()
        .and_then(|d| d.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    file_info_from_meta(&name, dirname.as_deref(), meta)
}

fn file_info_from_meta(
    name: &str,
    dirname: Option<&str>,
    meta: &std::fs::Metadata,
) -> crate::protocol::types::FileInfo {
    #[cfg(unix)]
    let (mode, uid, gid, modtime, dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.mode(), meta.uid(), meta.gid(), meta.mtime(), meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (mode, uid, gid, modtime, dev, ino) = {
        let modtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mode = if meta.is_dir() { 0o040755u32 } else { 0o100644u32 };
        (mode, 0u32, 0u32, modtime, 0u64, 0u64)
    };

    crate::protocol::types::FileInfo {
        name: name.to_string(),
        dirname: dirname.map(str::to_string),
        size: meta.len() as i64,
        modtime,
        mode,
        uid,
        gid,
        dev,
        ino,
        ..Default::default()
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    // Intercept --version and --help/-h before clap, so we can produce the
    // exact text that C rsync emits.  Note that C rsync only treats "-h"
    // as help when it is the sole argument; otherwise it means
    // --human-readable.  We follow the same rule.
    let raw: Vec<String> = std::env::args().collect();
    if raw.iter().any(|a| a == "--version" || a == "-V") {
        print_version();
        std::process::exit(0);
    }
    let lone_h = raw.len() == 2 && raw[1] == "-h";
    if raw.iter().any(|a| a == "--help") || lone_h {
        print_help();
        std::process::exit(0);
    }

    log_mod::log_init();

    // When built with the `debug-trace` feature, initialise the tracing
    // subscriber so that `rdebug!` events are emitted to stderr.
    // Control output with `RUST_LOG=rsync_rs=debug` (or any env-filter expr).
    #[cfg(feature = "debug-trace")]
    {
        use tracing_subscriber::EnvFilter;
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("rsync_rs=debug")),
            )
            .with_writer(std::io::stderr)
            .init();
    }

    let opts = match Options::try_parse() {
        Ok(o) => o,
        Err(e) => {
            // clap handles --help itself; for other parse errors print and use
            // C's syntax-error exit code so callers can distinguish bad usage.
            let kind = e.kind();
            e.print().ok();
            if matches!(kind,
                clap::error::ErrorKind::DisplayHelp |
                clap::error::ErrorKind::DisplayVersion) {
                std::process::exit(0);
            }
            std::process::exit(ExitCode::Syntax.as_i32());
        }
    };
    let mut opts = opts;
    opts.expand_archive();
    log_mod::set_verbosity(opts.verbose as i32);

    if opts.acls && !opts.server {
        eprintln!("rsync-rs: warning: --acls is not yet implemented; ACLs will not be preserved");
    }

    let result = if opts.daemon {
        daemon::run_daemon(&opts)
    } else if opts.server {
        run_server(&opts).map(|_| ())
    } else {
        run_client(&opts).map(|_| ())
    };

    if let Err(e) = result {
        let code = classify_error(&e);
        let role = if opts.server { "server" } else { "client" };
        // Emit error chain so the underlying cause is visible (matches the
        // C convention of letting the inner errno/strerror surface first).
        eprintln!("rsync: {:#}", e);
        eprintln!(
            "rsync error: {} (code {}) at main.rs({}) [{}={}]",
            code.description(),
            code.as_i32(),
            line!(),
            role,
            "3.4.2",
        );
        std::process::exit(code.as_i32());
    }
}

/// Classify an [`anyhow::Error`] into an [`ExitCode`] mirroring C rsync.
fn classify_error(e: &anyhow::Error) -> ExitCode {
    let s = format!("{:#}", e).to_ascii_lowercase();
    if s.contains("protocol") || s.contains("handshake") {
        ExitCode::Protocol
    } else if s.contains("connection") || s.contains("ssh") || s.contains("socket") {
        ExitCode::SocketIo
    } else if s.contains("no such file") || s.contains("permission denied") {
        ExitCode::FileIo
    } else if s.contains("vanished") {
        ExitCode::Vanished
    } else if s.contains("partial") {
        ExitCode::Partial
    } else if s.contains("timeout") {
        ExitCode::Timeout
    } else {
        ExitCode::FileIo
    }
}

