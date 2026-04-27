//! Core protocol types ported from rsync.h.
//! Idiomatic Rust equivalents — not slavish copies of the C structs.

#![allow(dead_code)]

// ── File type ────────────────────────────────────────────────────────────────

/// Broad file-type categories mirroring rsync's internal classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileType {
    #[default]
    Regular,
    Dir,
    Symlink,
    Device,
    Special,
}

impl FileType {
    /// Derive from a POSIX mode value.
    pub fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o040000 => FileType::Dir,
            0o120000 => FileType::Symlink,
            0o060000 | 0o020000 => FileType::Device, // block / char
            0o010000 | 0o140000 | 0o050000 => FileType::Special, // FIFO / socket / door
            _ => FileType::Regular,
        }
    }

    pub fn is_dir(self) -> bool { matches!(self, FileType::Dir) }
    pub fn is_regular(self) -> bool { matches!(self, FileType::Regular) }
    pub fn is_symlink(self) -> bool { matches!(self, FileType::Symlink) }
}

// ── Per-file info ─────────────────────────────────────────────────────────────

/// All metadata for a single file transferred in a file-list.
/// Corresponds to `file_struct` in rsync.h but uses owned Rust strings.
#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    /// Base filename (no directory component).
    pub name: String,
    /// Parent directory path, if known (None for top-level entries).
    pub dirname: Option<String>,
    /// Modification time (seconds since Unix epoch).
    pub modtime: i64,
    /// Sub-second part of mtime (nanoseconds; protocol 31+ `XMIT_MOD_NSEC`).
    pub mod_nsec: u32,
    /// File size in bytes.
    pub size: i64,
    /// POSIX mode bits (type + permissions).
    pub mode: u32,
    /// Internal FLAG_* bits (not transmitted; set locally).
    pub flags: u16,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// Symlink target (Some only when `FileType::Symlink`).
    pub link_target: Option<String>,
    /// Device major number (Some only for `FileType::Device`).
    pub rdev_major: u32,
    /// Device minor number (Some only for `FileType::Device`).
    pub rdev_minor: u32,
    /// Index of the first file in a hard-link cluster (`-1` = not hard-linked).
    pub hard_link_first_ndx: i32,
    /// Strong checksum (MD4/MD5/SHA-1 digest), if computed.
    pub checksum: Option<Vec<u8>>,
}

impl FileInfo {
    /// Return the full relative path (`dirname/name` or just `name`).
    pub fn path(&self) -> String {
        match &self.dirname {
            Some(d) if !d.is_empty() => format!("{}/{}", d, self.name),
            _ => self.name.clone(),
        }
    }

    /// Convenience accessor for the derived file type.
    pub fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode)
    }

    pub fn is_dir(&self) -> bool { self.file_type().is_dir() }
    pub fn is_regular(&self) -> bool { self.file_type().is_regular() }
    pub fn is_symlink(&self) -> bool { self.file_type().is_symlink() }
}

// ── File list ─────────────────────────────────────────────────────────────────

/// A list of files being transferred — corresponds to `file_list` in rsync.h.
/// The C version uses doubly-linked lists of chunks; here we keep a single Vec.
#[derive(Debug, Default)]
pub struct FileList {
    pub files: Vec<FileInfo>,
    /// Sorted order (indices into `files`); empty means use natural order.
    pub sorted: Vec<usize>,
    /// Index offset: the first `files[0]` has protocol index `ndx_start`.
    pub ndx_start: i32,
    /// Sequence number of this flist batch (incremental recursion).
    pub flist_num: i32,
    /// Index (in parent flist) of the directory that spawned this batch.
    pub parent_ndx: i32,
    pub in_progress: bool,
    pub to_redo: bool,
}

impl FileList {
    pub fn new() -> Self { Self::default() }

    pub fn len(&self) -> usize { self.files.len() }
    pub fn is_empty(&self) -> bool { self.files.is_empty() }

    /// Low / high indices (inclusive) within the current batch.
    pub fn low(&self) -> i32 { self.ndx_start }
    pub fn high(&self) -> i32 { self.ndx_start + self.files.len() as i32 - 1 }

    /// Retrieve a file by its protocol index.
    pub fn get_by_ndx(&self, ndx: i32) -> Option<&FileInfo> {
        let i = (ndx - self.ndx_start) as usize;
        self.files.get(i)
    }
}

// ── Block / rolling-checksum structures ──────────────────────────────────────

/// One block's checksums — mirrors `sum_buf` in rsync.h.
#[derive(Debug, Clone, Default)]
pub struct SumBuf {
    /// Byte offset of this block within the file.
    pub offset: i64,
    /// Number of bytes in this block.
    pub len: i32,
    /// Rolling (Adler-32–style) weak checksum.
    pub sum1: u32,
    /// Chain index used during matching (-1 = end of chain).
    pub chain: i32,
    /// Per-block flags (block is matched, etc.).
    pub flags: i16,
    /// Strong checksum bytes for this block (MD4 / MD5 slice stored inline).
    pub sum2: Vec<u8>,
}

/// The header sent on the wire before individual block checksums.
/// Corresponds to the four integers read by `receive_sums` in rsync.
#[derive(Debug, Clone, Copy, Default)]
pub struct SumHead {
    /// Number of blocks.
    pub count: i32,
    /// Block length (bytes per block, except possibly the last).
    pub blength: i32,
    /// Last block length in bytes (0 means same as blength).
    pub remainder: i32,
    /// Length of the per-block strong checksum in bytes.
    pub s2length: i32,
}

/// All checksums for one file — mirrors `sum_struct` in rsync.h.
#[derive(Debug, Default)]
pub struct SumStruct {
    /// Full length of the file these sums cover.
    pub flength: i64,
    /// Per-block checksum entries.
    pub sums: Vec<SumBuf>,
    /// Block length (same for every block except possibly the last).
    pub blength: i32,
    /// Last-block length (0 = same as blength).
    pub remainder: i32,
    /// Strong checksum byte length.
    pub s2length: i32,
}

impl SumStruct {
    pub fn count(&self) -> i32 { self.sums.len() as i32 }

    pub fn head(&self) -> SumHead {
        SumHead {
            count: self.count(),
            blength: self.blength,
            remainder: self.remainder,
            s2length: self.s2length,
        }
    }
}

// ── Memory-mapped file window ────────────────────────────────────────────────

/// State for a sliding window over a file — mirrors `map_struct` in rsync.h.
/// The Rust version stores the raw fd and delegates actual mmap to callers.
#[derive(Debug)]
pub struct MapStruct {
    pub file_size: i64,
    /// Byte offset of the current window start within the file.
    pub p_offset: i64,
    /// fd-relative offset that was actually mapped.
    pub p_fd_offset: i64,
    /// Mapped bytes; empty if no window is currently active.
    pub data: Vec<u8>,
    /// Requested window size.
    pub def_window_size: i32,
    /// Number of valid bytes in `data`.
    pub p_len: i32,
    pub fd: i32,
    pub status: i32,
}

impl MapStruct {
    pub fn new(fd: i32, file_size: i64, window_size: i32) -> Self {
        Self {
            file_size,
            p_offset: 0,
            p_fd_offset: 0,
            data: Vec::new(),
            def_window_size: window_size,
            p_len: 0,
            fd,
            status: 0,
        }
    }
}

// ── Transfer statistics ───────────────────────────────────────────────────────

/// Accumulated statistics for an rsync run — mirrors `struct stats` in rsync.h.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_size: i64,
    pub total_transferred_size: i64,
    pub total_written: i64,
    pub total_read: i64,
    pub literal_data: i64,
    pub matched_data: i64,
    /// Time (ms) spent building the file list.
    pub flist_buildtime: i64,
    /// Time (ms) spent transferring the file list.
    pub flist_xfertime: i64,
    pub flist_size: i64,

    pub num_files: i32,
    pub num_dirs: i32,
    pub num_symlinks: i32,
    pub num_devices: i32,
    pub num_specials: i32,

    pub created_files: i32,
    pub created_dirs: i32,
    pub created_symlinks: i32,
    pub created_devices: i32,
    pub created_specials: i32,

    pub deleted_files: i32,
    pub deleted_dirs: i32,
    pub deleted_symlinks: i32,
    pub deleted_devices: i32,
    pub deleted_specials: i32,

    pub xferred_files: i32,
}

// ── Filter rules ──────────────────────────────────────────────────────────────

/// A single include/exclude rule — mirrors `filter_rule` / `filter_struct`.
#[derive(Debug, Clone)]
pub struct FilterRule {
    /// The glob pattern or path string.
    pub pattern: String,
    /// RULE_* flags controlling match behaviour.
    pub rflags: u32,
    /// Number of '/' characters in the pattern (for anchoring logic).
    pub slash_cnt: i32,
    /// If this rule is a merge-file directive, the list it references.
    pub mergelist: Option<Box<FilterRuleList>>,
    /// When non-zero, this rule should be elided (skipped) in output.
    pub elide: u8,
}

impl FilterRule {
    pub fn new(pattern: impl Into<String>, rflags: u32) -> Self {
        let pattern = pattern.into();
        let slash_cnt = pattern.chars().filter(|&c| c == '/').count() as i32;
        Self { pattern, rflags, slash_cnt, mergelist: None, elide: 0 }
    }
}

/// A linked list of filter rules — mirrors `filter_list_struct`.
#[derive(Debug, Clone, Default)]
pub struct FilterRuleList {
    pub rules: Vec<FilterRule>,
    /// Human-readable label used in debug output (e.g. "exclude list").
    pub debug_type: String,
}

impl FilterRuleList {
    pub fn new(debug_type: impl Into<String>) -> Self {
        Self { rules: Vec::new(), debug_type: debug_type.into() }
    }

    pub fn push(&mut self, rule: FilterRule) { self.rules.push(rule); }
    pub fn is_empty(&self) -> bool { self.rules.is_empty() }
}

// ── Filter rule flag constants ────────────────────────────────────────────────

pub const RULE_EXCLUDE: u32 = 1 << 0;
pub const RULE_INCLUDE: u32 = 1 << 1;
pub const RULE_CLEAR: u32 = 1 << 2;
pub const RULE_MERGE_FILE: u32 = 1 << 3;
pub const RULE_PERDIR_MERGE: u32 = 1 << 4;
pub const RULE_EXCLUDE_PERISHABLE: u32 = 1 << 5;
pub const RULE_ANCHORED: u32 = 1 << 6;
pub const RULE_WILD: u32 = 1 << 7;
pub const RULE_WILD2: u32 = 1 << 8;
pub const RULE_WILD2_PREFIX: u32 = 1 << 9;
pub const RULE_WILD3: u32 = 1 << 10;
pub const RULE_DIRECTORY: u32 = 1 << 11;
pub const RULE_ABS_PATH: u32 = 1 << 12;
pub const RULE_NEGATE: u32 = 1 << 13;
pub const RULE_CVS_IGNORE: u32 = 1 << 14;
pub const RULE_SENDER_SIDE: u32 = 1 << 15;
pub const RULE_RECEIVER_SIDE: u32 = 1 << 16;
pub const RULE_RISK_SOURCE: u32 = 1 << 17;
pub const RULE_LIMIT_XATTR: u32 = 1 << 18;
pub const RULE_NEVER_EXCLUDE: u32 = 1 << 19;


