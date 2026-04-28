pub mod checksum;
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
) -> Result<(bool, i32, Option<String>)> {
    if protocol < 30 {
        return Ok((false, 0, None));
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

    Ok((do_varint, checksum_seed, compression_choice))
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

/// Client-side compat setup. Returns `(do_varint, checksum_seed, compression_choice)`.
fn setup_compat_client<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    protocol: u32,
    do_compression: bool,
) -> Result<(bool, i32, Option<String>)> {
    if protocol < 30 {
        return Ok((false, 0, None));
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

    Ok((do_varint, checksum_seed, compression_choice))
}

// ── Server-mode dispatch ──────────────────────────────────────────────────────

/// Receive and discard the filter-rule list sent by the remote peer.
///
/// The C rsync client always sends a filter list (terminated by `write_int(0)`)
/// before the server sends or receives the file list.  We must consume it so
/// we don't misinterpret the first rule as a generator index or a file-list
/// entry.
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
    let (do_varint_flist, checksum_seed, compression_choice) =
        if protocol >= 30 {
            setup_compat(&mut reader, &mut writer, opts, protocol, do_compression)?
        } else {
            (false, 0, None)
        };
    let _ = do_varint_flist; // stored for future flist encoding selection
    let use_zlib = matches!(compression_choice.as_deref(), Some("zlib"));

    let csum_ct = crate::checksum::strong::ChecksumType::for_protocol(protocol, false);
    let rsync_ct = strong_to_csum_type(csum_ct);
    let checksum_len = if opts.checksum { csum_ct.digest_len() } else { 0 };

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

        // Step 4: Consume filter rules from client (terminated by int(0)).
        crate::rdebug!("[rsync-rs] waiting for filter list...");
        recv_filter_list(&mut reader).context("recv_filter_list")?;
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
                walk_source_dir(path, "", recursive, &mut flist);
            } else if let Ok(meta) = std::fs::metadata(path) {
                let name =
                    path.file_name().and_then(|n| n.to_str()).unwrap_or(src).to_string();
                flist.files.push(file_info_from_meta(&name, None, &meta));
            }
        }
        crate::flist::flist_sort(&mut flist);
        crate::rdebug!("[rsync-rs] flist has {} files", flist.files.len());
        for (i, fi) in flist.files.iter().enumerate() {
            crate::rdebug!("[rsync-rs]   sorted[{}] = {:?}", i, fi.path());
        }

        // Step 7: Send file list (through multiplexed output).
        crate::rdebug!("[rsync-rs] sending file list...");
        crate::flist::send_file_list_ex(&mut writer, &flist, protocol, checksum_len, 0, preserve)
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

        // NOTE: C's send_filter_list skips sending entirely when am_sender &&
        // !receiver_wants_list (no --delete / --prune-empty-dirs). For -av
        // push there is no filter list on the wire, so we must NOT block
        // reading one. (TODO: enable when delete-mode is supported.)

        // Server-receiver: server_flags / preserve already computed (Step 1b).
        let preserve = server_flags.to_preserve();

        // Step 5: Receive file list from client.
        let flist = crate::flist::recv_file_list_ex(&mut reader, protocol, checksum_len, preserve.uid, preserve.gid)
            .context("recv_file_list")?;
        crate::rdebug!("[rsync-rs] receiver: flist done ({} entries)", flist.files.len());
        // (The checksum seed was already exchanged during setup_compat; the C
        // client sender does NOT send a second seed after the flist.)

        let dest_dir =
            opts.args.last().map(std::path::Path::new).unwrap_or(std::path::Path::new("."));
        let stats = pipeline::receiver::run_server_receiver(
            &mut reader, &mut writer, &flist, dest_dir, rsync_ct, protocol, checksum_seed,
            use_zlib, opts.inplace, opts.itemize_changes,
        )
        .context("server-receiver run")?;
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
        if opts.stats || opts.verbose > 0 {
            print_stats(&report.stats, start.elapsed().as_secs_f64(), opts.stats, opts.dry_run, false);
        }
        return Ok(report.stats);
    };

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
    let (_do_varint, checksum_seed, compression_choice) = if protocol >= 30 {
        crate::rdebug!("[rsync-rs client] starting setup_compat_client...");
        let r = setup_compat_client(&mut stdout_pipe, &mut stdin_pipe, protocol, opts.compress)?;
        crate::rdebug!("[rsync-rs client] setup_compat_client done, seed={}", r.1);
        r
    } else {
        (false, 0, None)
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
        // Server-side: server-sender's OUT is muxed (proto>=23); server-receiver's
        // OUT is muxed (in do_server_recv). So our IN must demultiplex.
        reader.enable();
        // Server-sender's IN is muxed (proto>=31 with need_messages_from_generator).
        // Server-receiver's IN is muxed (proto>=30). So our OUT must mux too.
        writer.enable();
    }

    let preserve = crate::flist::send::Preserve {
        uid: opts.owner || opts.archive,
        gid: opts.group || opts.archive,
        times: opts.times || opts.archive,
        devices: false,
    };

    let stats = if is_push {
        // Client-sender (push). C's exclude.c::send_filter_list line 1650
        // suppresses output when (am_sender && !receiver_wants_list); for
        // plain -av there's no filter list on the wire.

        // Build flist from local sources by walking the tree.
        let mut flist = crate::protocol::types::FileList::new();
        for src in &sources {
            let p = std::path::Path::new(src);
            let recursive = opts.recursive;
            if p.is_dir() {
                walk_source_dir(p, "", recursive, &mut flist);
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

        if opts.verbose >= 1 {
            println!("sending incremental file list");
        }
        crate::flist::send::send_file_list_ex(&mut writer, &flist, protocol, checksum_len, 0, preserve)
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

        // C client-sender: handle_stats() does nothing for am_sender client.
        // read_final_goodbye reads NDX_DONE; for am_sender + proto>=31 writes
        // NDX_DONE, then reads another.
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
        // Client-receiver (pull). C sends an empty filter list (write_int(0)).
        crate::rdebug!("[rsync-rs client] sending filter list terminator...");
        write_int(&mut writer, 0)?;
        writer.flush().ok();
        crate::rdebug!("[rsync-rs client] filter list sent, waiting for flist...");

        if opts.verbose >= 1 {
            println!("receiving incremental file list");
        }
        let flist = crate::flist::recv_file_list_ex(&mut reader, protocol, checksum_len, preserve.uid, preserve.gid)
            .context("recv_file_list")?;
        crate::rdebug!("[rsync-rs client] received flist with {} entries", flist.files.len());

        let dest_path = std::path::Path::new(&dest);
        let _ = std::fs::create_dir_all(dest_path);

        crate::rdebug!("[rsync-rs client] starting receiver pipeline...");
        let mut stats = pipeline::receiver::run_server_receiver(
            &mut reader, &mut writer, &flist, dest_path, rsync_ct, protocol, checksum_seed,
            use_zlib, opts.inplace, opts.itemize_changes,
        ).context("client-receiver run")?;
        crate::rdebug!("[rsync-rs client] receiver pipeline done");

        // Read stats sent by the server-sender (handle_stats writes 3 or 5 varlongs).
        if protocol >= 29 {
            let total_written = crate::io::varint::read_varlong(&mut reader, 3).unwrap_or(0);
            let total_read = crate::io::varint::read_varlong(&mut reader, 3).unwrap_or(0);
            let total_size = crate::io::varint::read_varlong(&mut reader, 3).unwrap_or(0);
            let _ = crate::io::varint::read_varlong(&mut reader, 3).ok(); // flist_buildtime
            let _ = crate::io::varint::read_varlong(&mut reader, 3).ok(); // flist_xfertime
            stats.total_written = total_written;
            stats.total_read = total_read;
            stats.total_size = total_size;
        }
        stats
    };

    drop(writer);
    drop(reader);
    let _ = ssh_child.wait();
    if opts.stats || opts.verbose > 0 {
        print_stats(&stats, start.elapsed().as_secs_f64(), opts.stats, opts.dry_run, false);
    }
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
        if ft.is_symlink() {
            let mut fi = file_info_from_meta(&name, dirname.as_deref(), &meta);
            if let Ok(target) = std::fs::read_link(entry.path()) {
                fi.link_target = Some(target.to_string_lossy().into_owned());
            }
            flist.files.push(fi);
        } else if ft.is_dir() {
            flist.files.push(file_info_from_meta(&name, dirname.as_deref(), &meta));
            if recursive {
                walk_source_dir(&entry.path(), &rel, true, flist);
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
    let (mode, uid, gid, modtime) = {
        use std::os::unix::fs::MetadataExt;
        (meta.mode(), meta.uid(), meta.gid(), meta.mtime())
    };
    #[cfg(not(unix))]
    let (mode, uid, gid, modtime) = {
        let modtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mode = if meta.is_dir() { 0o040755u32 } else { 0o100644u32 };
        (mode, 0u32, 0u32, modtime)
    };

    crate::protocol::types::FileInfo {
        name: name.to_string(),
        dirname: dirname.map(str::to_string),
        size: meta.len() as i64,
        modtime,
        mode,
        uid,
        gid,
        ..Default::default()
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    // Check --version before clap so we control the exact output format.
    if std::env::args().any(|a| a == "--version") {
        print_version();
        std::process::exit(0);
    }

    log_mod::log_init();

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

