//! Batch file support (`--write-batch` / `--read-batch`).
//!
//! ## Format
//!
//! ```text
//! +------------------+
//! | magic "RSYNBAT1" |  8 bytes
//! +------------------+
//! | stream_flags     |  4-byte LE u32  (option bitmap, see BFLAG_* consts)
//! | checksum_seed    |  4-byte LE i32
//! | protocol         |  4-byte LE u32
//! +------------------+
//! | flist length     |  4-byte LE u32  (length of following flist blob)
//! | flist blob       |  wire-format flist (send_file_list_ex output)
//! +------------------+
//! | transfer records |  repeated:
//! |   tag u8: 0x01=FILE | 0xFF=END
//! |   ndx  i32 LE
//! |   len  u64 LE
//! |   data [u8; len]   (whole file content)
//! +------------------+
//! ```
//!
//! The format is rsync-rs native; it is **not** byte-compatible with C rsync
//! batch files (which use protocol 29 internals).

use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::flist::send::{Preserve, send_file_list_ex};
use crate::options::Options;
use crate::protocol::constants::{CF_VARINT_FLIST_FLAGS, PROTOCOL_VERSION};
use crate::protocol::types::FileList;

// ── Constants ──────────────────────────────────────────────────────────────────

pub const BATCH_MAGIC: &[u8; 8] = b"RSYNBAT1";

/// Stream-flags bitmap — mirrors C rsync's batch stream flags.
pub const BFLAG_RECURSE: u32 = 1 << 0;
pub const BFLAG_OWNER: u32 = 1 << 1;
pub const BFLAG_GROUP: u32 = 1 << 2;
pub const BFLAG_LINKS: u32 = 1 << 3;
pub const BFLAG_DEVICES: u32 = 1 << 4;
pub const BFLAG_HARD_LINKS: u32 = 1 << 5;
pub const BFLAG_CHECKSUM: u32 = 1 << 6;
pub const BFLAG_COMPRESS: u32 = 1 << 8;

const TAG_FILE: u8 = 0x01;
const TAG_END: u8 = 0xFF;

// ── Stream-flags helpers ───────────────────────────────────────────────────────

/// Build the stream-flags bitmap from the current options.
pub fn stream_flags(opts: &Options) -> u32 {
    let mut f = 0u32;
    if opts.recursive || opts.archive { f |= BFLAG_RECURSE; }
    if opts.owner    || opts.archive { f |= BFLAG_OWNER; }
    if opts.group    || opts.archive { f |= BFLAG_GROUP; }
    if opts.links    || opts.archive { f |= BFLAG_LINKS; }
    if opts.devices  || opts.archive { f |= BFLAG_DEVICES; }
    if opts.hard_links               { f |= BFLAG_HARD_LINKS; }
    if opts.checksum                 { f |= BFLAG_CHECKSUM; }
    if opts.compress                 { f |= BFLAG_COMPRESS; }
    f
}

/// Apply stream flags read from a batch file back onto an options struct.
pub fn apply_stream_flags(flags: u32, opts: &mut Options) {
    opts.recursive  = flags & BFLAG_RECURSE    != 0;
    opts.owner      = flags & BFLAG_OWNER      != 0;
    opts.group      = flags & BFLAG_GROUP      != 0;
    opts.links      = flags & BFLAG_LINKS      != 0;
    opts.devices    = flags & BFLAG_DEVICES    != 0;
    opts.hard_links = flags & BFLAG_HARD_LINKS != 0;
    opts.checksum   = flags & BFLAG_CHECKSUM   != 0;
    opts.compress   = flags & BFLAG_COMPRESS   != 0;
}

// ── BatchWriter ────────────────────────────────────────────────────────────────

/// Writes a batch file incrementally.
pub struct BatchWriter {
    inner: BufWriter<File>,
}

impl BatchWriter {
    /// Open (or create) `path` and write the batch file header.
    pub fn create(path: &str, opts: &Options) -> Result<Self> {
        let f = create_file(path, 0o600)?;
        let mut w = BufWriter::new(f);

        // Magic + stream flags (LE u32) + checksum seed (i32) + protocol (u32)
        w.write_all(BATCH_MAGIC)?;
        w.write_all(&stream_flags(opts).to_le_bytes())?;
        w.write_all(&0i32.to_le_bytes())?;
        w.write_all(&PROTOCOL_VERSION.to_le_bytes())?;

        Ok(BatchWriter { inner: w })
    }

    /// Write the serialized file list.
    pub fn write_flist(&mut self, flist: &FileList, preserve_uid: bool, preserve_gid: bool) -> Result<()> {
        let mut buf: Vec<u8> = Vec::new();
        let preserve = Preserve { uid: preserve_uid, gid: preserve_gid, times: true, devices: false };
        send_file_list_ex(&mut buf, flist, PROTOCOL_VERSION, 4, 0, preserve, CF_VARINT_FLIST_FLAGS)?;

        let len = buf.len() as u32;
        self.inner.write_all(&len.to_le_bytes())?;
        self.inner.write_all(&buf)?;
        Ok(())
    }

    /// Write one file's content to the batch.
    pub fn write_file(&mut self, ndx: i32, content: &[u8]) -> Result<()> {
        self.inner.write_all(&[TAG_FILE])?;
        self.inner.write_all(&ndx.to_le_bytes())?;
        self.inner.write_all(&(content.len() as u64).to_le_bytes())?;
        self.inner.write_all(content)?;
        Ok(())
    }

    /// Write the END marker and flush.
    pub fn finish(&mut self) -> Result<()> {
        self.inner.write_all(&[TAG_END])?;
        self.inner.flush()?;
        Ok(())
    }
}

// ── BatchReader ────────────────────────────────────────────────────────────────

/// Reads a batch file.
pub struct BatchReader {
    inner: BufReader<File>,
    pub stream_flags: u32,
    pub checksum_seed: i32,
    pub protocol: u32,
}

impl BatchReader {
    /// Open `path` and verify the header.
    pub fn open(path: &str) -> Result<Self> {
        let f = File::open(path)
            .with_context(|| format!("open batch file {:?}", path))?;
        let mut r = BufReader::new(f);

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic).context("read batch magic")?;
        if &magic != BATCH_MAGIC {
            bail!("not a valid rsync-rs batch file: {:?}", path);
        }

        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let stream_flags = u32::from_le_bytes(buf4);
        r.read_exact(&mut buf4)?;
        let checksum_seed = i32::from_le_bytes(buf4);
        r.read_exact(&mut buf4)?;
        let protocol = u32::from_le_bytes(buf4);

        Ok(BatchReader { inner: r, stream_flags, checksum_seed, protocol })
    }

    /// Read the serialized file list.
    pub fn read_flist(&mut self) -> Result<FileList> {
        let mut buf4 = [0u8; 4];
        self.inner.read_exact(&mut buf4)?;
        let len = u32::from_le_bytes(buf4) as usize;
        let mut buf = vec![0u8; len];
        self.inner.read_exact(&mut buf)?;
        let mut cur = std::io::Cursor::new(buf);
        crate::flist::recv_file_list_ex(
            &mut cur,
            self.protocol,
            4,     // checksum_len
            false, // preserve_uid (not needed for playback)
            false, // preserve_gid
            CF_VARINT_FLIST_FLAGS,
        )
    }

    /// Read the next transfer record.  Returns `None` at END.
    pub fn read_record(&mut self) -> Result<Option<(i32, Vec<u8>)>> {
        let mut tag = [0u8];
        self.inner.read_exact(&mut tag)?;
        if tag[0] == TAG_END {
            return Ok(None);
        }
        if tag[0] != TAG_FILE {
            bail!("unexpected batch record tag 0x{:02x}", tag[0]);
        }
        let mut buf4 = [0u8; 4];
        self.inner.read_exact(&mut buf4)?;
        let ndx = i32::from_le_bytes(buf4);
        let mut buf8 = [0u8; 8];
        self.inner.read_exact(&mut buf8)?;
        let len = u64::from_le_bytes(buf8) as usize;
        let mut data = vec![0u8; len];
        self.inner.read_exact(&mut data)?;
        Ok(Some((ndx, data)))
    }
}

// ── Shell script generation ────────────────────────────────────────────────────

/// Write the companion `.sh` script that replays the batch.
pub fn write_shell_script(batch_path: &str, opts: &Options) -> Result<()> {
    let sh_path = format!("{}.sh", batch_path);
    let mut sh = create_file(&sh_path, 0o755)?;

    writeln!(sh, "#!/bin/sh")?;
    writeln!(sh, "# rsync-rs batch script — replay with: sh {} [DEST]", sh_path)?;
    writeln!(sh)?;
    write!(sh, "rsync-rs")?;

    if opts.archive { write!(sh, " -a")?; } else {
        if opts.recursive  { write!(sh, " -r")?; }
        if opts.links      { write!(sh, " -l")?; }
        if opts.perms      { write!(sh, " -p")?; }
        if opts.times      { write!(sh, " -t")?; }
        if opts.group      { write!(sh, " -g")?; }
        if opts.owner      { write!(sh, " -o")?; }
    }
    for _ in 0..opts.verbose { write!(sh, " -v")?; }
    if opts.checksum { write!(sh, " --checksum")?; }
    if opts.compress { write!(sh, " --compress")?; }
    if opts.delete   { write!(sh, " --delete")?; }
    for pat in &opts.exclude { write!(sh, " --exclude={}", shell_quote(pat))?; }
    write!(sh, " --read-batch={}", shell_quote(batch_path))?;
    sh.write_all(b" \"${1:-DEST}\"\n")?;
    sh.flush()?;
    Ok(())
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "_-./:=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Create a file with given mode bits (Unix) or default (Windows).
fn create_file(path: &str, _mode: u32) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(_mode);
    }
    opts.open(path).with_context(|| format!("create file {:?}", path))
}

// ── Top-level: write_batch ────────────────────────────────────────────────────

/// Capture a local sync to a batch file.
///
/// `flist` must already be built (and sorted) by the caller.
/// `sources` are the local source paths (used to read file content).
pub fn run_write_batch(
    opts: &Options,
    flist: &FileList,
    sources: &[String],
    batch_path: &str,
) -> Result<()> {
    let preserve_uid = opts.owner || opts.archive;
    let preserve_gid = opts.group || opts.archive;

    let mut bw = BatchWriter::create(batch_path, opts)?;
    bw.write_flist(flist, preserve_uid, preserve_gid)?;

    // Write regular file contents (whole-file; no delta in batch).
    for (ndx, fi) in flist.files.iter().enumerate() {
        if !fi.is_regular() {
            continue;
        }
        let rel = fi.path();
        // Find the file by searching under each source path.
        let content = sources.iter().find_map(|src| {
            let candidate = Path::new(src).join(&rel);
            if candidate.is_file() {
                fs::read(&candidate).ok()
            } else {
                // Also try src itself when the source is a single file.
                let direct = Path::new(src);
                if direct.is_file() && direct.file_name().and_then(|n| n.to_str()) == Some(&fi.name) {
                    fs::read(direct).ok()
                } else {
                    None
                }
            }
        });
        if let Some(data) = content {
            bw.write_file(ndx as i32, &data)?;
        }
    }
    bw.finish()?;
    write_shell_script(batch_path, opts)?;

    if opts.verbose >= 1 {
        let n_files = flist.files.iter().filter(|f| f.is_regular()).count();
        println!("batch file written: {}", batch_path);
        println!("batch shell script: {}.sh", batch_path);
        println!("captured {} file{}", n_files, if n_files == 1 { "" } else { "s" });
    }
    Ok(())
}

// ── Top-level: read_batch ─────────────────────────────────────────────────────

/// Apply a batch file to a local destination.
pub fn run_read_batch(opts: &Options, batch_path: &str, dest: &str) -> Result<()> {
    let mut br = BatchReader::open(batch_path)?;

    // (Stream flags recorded in the batch; apply them if caller passes &mut opts)
    // For now we read the flags but apply them locally for display purposes only.
    let _flags = br.stream_flags;

    let dest_path = Path::new(dest);
    if !opts.dry_run {
        fs::create_dir_all(dest_path)
            .with_context(|| format!("create dest {:?}", dest_path))?;
    }

    let flist = br.read_flist()?;

    if opts.verbose >= 1 {
        println!("receiving file list ...");
        println!("{} file{} to consider",
            flist.files.len(), if flist.files.len() == 1 { "" } else { "s" });
    }

    // Pre-create destination directories.
    for fi in &flist.files {
        if fi.is_dir() {
            let name = fi.path();
            if name == "." { continue; }
            let dest_dir = dest_path.join(&name);
            if !opts.dry_run {
                fs::create_dir_all(&dest_dir)
                    .with_context(|| format!("create_dir_all {:?}", dest_dir))?;
            }
        }
    }

    // Apply transfers.
    let mut sent = 0u64;
    while let Some((ndx, data)) = br.read_record()? {
        let fi = flist.files.get(ndx as usize)
            .ok_or_else(|| anyhow::anyhow!("batch ndx {} out of range", ndx))?;
        let name = fi.path();
        let final_dest = dest_path.join(&name);

        if opts.verbose >= 1 {
            println!("{}", name);
        }
        if opts.dry_run {
            continue;
        }
        if let Some(parent) = final_dest.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&final_dest, &data)
            .with_context(|| format!("write {:?}", final_dest))?;
        sent += data.len() as u64;
    }

    if opts.verbose >= 1 {
        println!("\nsent 0 bytes  received {} bytes", sent);
        println!("total size is {}  speedup is 1.00", sent);
    }

    Ok(())
}
