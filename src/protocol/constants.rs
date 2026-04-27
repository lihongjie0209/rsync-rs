/// rsync protocol version constants and flags (from rsync.h).

// ── Protocol versions ──────────────────────────────────────────────────────
pub const PROTOCOL_VERSION: u32 = 31; // Target protocol we speak
pub const SUBPROTOCOL_VERSION: u32 = 0; // 0 = final release
pub const MIN_PROTOCOL_VERSION: u32 = 20;
pub const OLD_PROTOCOL_VERSION: u32 = 25;
pub const MAX_PROTOCOL_VERSION: u32 = 40;

// ── Network ────────────────────────────────────────────────────────────────
pub const RSYNC_PORT: u16 = 873;
pub const RSYNC_NAME: &str = "rsync";
pub const URL_PREFIX: &str = "rsync://";
pub const SYMLINK_PREFIX: &str = "/rsyncd-munged/";

// ── Block / transfer sizes ─────────────────────────────────────────────────
pub const BLOCK_SIZE: u32 = 700;
pub const WRITE_SIZE: usize = 32 * 1024;
pub const CHUNK_SIZE: usize = 32 * 1024;
pub const MAX_MAP_SIZE: usize = 256 * 1024;
pub const IO_BUFFER_SIZE: usize = 32 * 1024;
pub const MAX_BLOCK_SIZE: i32 = 1 << 17;
pub const OLD_MAX_BLOCK_SIZE: i32 = 1 << 29;
pub const SPARSE_WRITE_SIZE: usize = 1024;

// ── Checksum ───────────────────────────────────────────────────────────────
pub const SUM_LENGTH: usize = 16;
pub const SHORT_SUM_LENGTH: usize = 2;
pub const BLOCKSUM_BIAS: u32 = 10;
pub const CHAR_OFFSET: u32 = 0; // Non-zero would be incompatible
pub const CSUM_CHUNK: usize = 64; // MD4 block size
pub const MAX_DIGEST_LEN: usize = 64; // Enough for SHA-512

/// Checksum type IDs (used in protocol negotiation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsumType {
    None = 0,
    Md4Archaic = 1, // protocols < 21
    Md4Busted = 2,  // protocols 21–26 (buggy tail handling)
    Md4Old = 3,     // protocols 27–29
    Md4 = 4,        // protocols 30+
    Md5 = 5,
    Sha1 = 6,
    Sha256 = 7,
    Sha512 = 8,
    Xxh64 = 9,
    Xxh3_64 = 10,
    Xxh3_128 = 11,
}

/// Lengths of strong checksums by type.
pub const MD4_DIGEST_LEN: usize = 16;
pub const MD5_DIGEST_LEN: usize = 16;

// ── Multiplexed I/O ─────────────────────────────────────────────────────────
/// The base offset added to message codes in the 4-byte multiplex header.
pub const MPLEX_BASE: u8 = 7;

// ── XMIT flags (file list transfer) ────────────────────────────────────────
// These flags are only used during the flist transfer.
pub const XMIT_TOP_DIR: u32 = 1 << 0;
pub const XMIT_SAME_MODE: u32 = 1 << 1;
pub const XMIT_EXTENDED_FLAGS: u32 = 1 << 2; // protocols 28+
pub const XMIT_SAME_RDEV_PRE28: u32 = 1 << 2; // protocols 20-27
pub const XMIT_SAME_UID: u32 = 1 << 3;
pub const XMIT_SAME_GID: u32 = 1 << 4;
pub const XMIT_SAME_NAME: u32 = 1 << 5;
pub const XMIT_LONG_NAME: u32 = 1 << 6;
pub const XMIT_SAME_TIME: u32 = 1 << 7;
// Extended flags (XMIT_EXTENDED_FLAGS must be set)
pub const XMIT_SAME_RDEV_MAJOR: u32 = 1 << 8; // devices only
pub const XMIT_NO_CONTENT_DIR: u32 = 1 << 8; // dirs only (protocols 30+)
pub const XMIT_HLINKED: u32 = 1 << 9;         // protocols 28+ (non-dirs)
pub const XMIT_SAME_DEV_PRE30: u32 = 1 << 10; // protocols 28-29
pub const XMIT_USER_NAME_FOLLOWS: u32 = 1 << 10; // protocols 30+
pub const XMIT_RDEV_MINOR_8_PRE30: u32 = 1 << 11; // protocols 28-29
pub const XMIT_GROUP_NAME_FOLLOWS: u32 = 1 << 11; // protocols 30+
pub const XMIT_HLINK_FIRST: u32 = 1 << 12;    // protocols 30+ (HLINKED only)
pub const XMIT_IO_ERROR_ENDLIST: u32 = 1 << 12; // protocols 31+ (w/EXTENDED_FLAGS)
pub const XMIT_MOD_NSEC: u32 = 1 << 13;       // protocols 31+
pub const XMIT_SAME_ATIME: u32 = 1 << 14;     // any protocol
pub const XMIT_CRTIME_EQ_MTIME: u32 = 1 << 17; // any protocol (varint flags)

// ── Live flist FLAG_* bits ─────────────────────────────────────────────────
pub const FLAG_TOP_DIR: u16 = 1 << 0;
pub const FLAG_FILE_SENT: u16 = 1 << 1;
pub const FLAG_CONTENT_DIR: u16 = 1 << 2;
pub const FLAG_MOUNT_DIR: u16 = 1 << 3;
pub const FLAG_SKIP_HLINK: u16 = 1 << 3;
pub const FLAG_DUPLICATE: u16 = 1 << 4;
pub const FLAG_MISSING_DIR: u16 = 1 << 4;
pub const FLAG_HLINKED: u16 = 1 << 5;
pub const FLAG_HLINK_FIRST: u16 = 1 << 6;
pub const FLAG_IMPLIED_DIR: u16 = 1 << 6;
pub const FLAG_HLINK_LAST: u16 = 1 << 7;
pub const FLAG_HLINK_DONE: u16 = 1 << 8;
pub const FLAG_LENGTH64: u16 = 1 << 9;
pub const FLAG_SKIP_GROUP: u16 = 1 << 10;
pub const FLAG_TIME_FAILED: u16 = 1 << 11;
pub const FLAG_MOD_NSEC: u16 = 1 << 12;
pub const FLAG_GOT_DIR_FLIST: u16 = 1 << 13;

// ── NDX special values (file index) ────────────────────────────────────────
pub const NDX_DONE: i32 = -1;
pub const NDX_FLIST_EOF: i32 = -2;
pub const NDX_DEL_STATS: i32 = -3;
pub const NDX_FLIST_OFFSET: i32 = -101;

// ── I/O error bits ─────────────────────────────────────────────────────────
pub const IOERR_GENERAL: u32 = 1 << 0;
pub const IOERR_VANISHED: u32 = 1 << 1;
pub const IOERR_DEL_LIMIT: u32 = 1 << 2;

// ── Delete flags ──────────────────────────────────────────────────────────
pub const DEL_NO_UID_WRITE: u32 = 1 << 0;
pub const DEL_RECURSE: u32 = 1 << 1;
pub const DEL_DIR_IS_EMPTY: u32 = 1 << 2;
pub const DEL_FOR_FILE: u32 = 1 << 3;
pub const DEL_FOR_DIR: u32 = 1 << 4;
pub const DEL_FOR_SYMLINK: u32 = 1 << 5;
pub const DEL_FOR_DEVICE: u32 = 1 << 6;
pub const DEL_FOR_SPECIAL: u32 = 1 << 7;
pub const DEL_FOR_BACKUP: u32 = 1 << 8;

// ── Itemize-change bits ────────────────────────────────────────────────────
pub const ITEM_REPORT_ATIME: u32 = 1 << 0;
pub const ITEM_REPORT_CHANGE: u32 = 1 << 1;
pub const ITEM_REPORT_SIZE: u32 = 1 << 2;
pub const ITEM_REPORT_TIMEFAIL: u32 = 1 << 2; // symlinks only
pub const ITEM_REPORT_TIME: u32 = 1 << 3;
pub const ITEM_REPORT_PERMS: u32 = 1 << 4;
pub const ITEM_REPORT_OWNER: u32 = 1 << 5;
pub const ITEM_REPORT_GROUP: u32 = 1 << 6;
pub const ITEM_REPORT_ACL: u32 = 1 << 7;
pub const ITEM_REPORT_XATTR: u32 = 1 << 8;
pub const ITEM_REPORT_CRTIME: u32 = 1 << 10;
pub const ITEM_BASIS_TYPE_FOLLOWS: u32 = 1 << 11;
pub const ITEM_XNAME_FOLLOWS: u32 = 1 << 12;
pub const ITEM_IS_NEW: u32 = 1 << 13;
pub const ITEM_LOCAL_CHANGE: u32 = 1 << 14;
pub const ITEM_TRANSFER: u32 = 1 << 15;
pub const ITEM_MISSING_DATA: u32 = 1 << 16;
pub const ITEM_DELETED: u32 = 1 << 17;
pub const ITEM_MATCHED: u32 = 1 << 18;

// ── Compat flags (protocol 30+) ────────────────────────────────────────────
pub const CF_INC_RECURSE: u32 = 1 << 0;
pub const CF_SYMLINK_TIMES: u32 = 1 << 1;
pub const CF_SYMLINK_ICONV: u32 = 1 << 2;
pub const CF_SAFE_FLIST: u32 = 1 << 3;
pub const CF_AVOID_XATTR_OPTIM: u32 = 1 << 4;
pub const CF_CHKSUM_SEED_FIX: u32 = 1 << 5; // proper_seed_order
pub const CF_INPLACE_PARTIAL_DIR: u32 = 1 << 6;
pub const CF_VARINT_FLIST_FLAGS: u32 = 1 << 7;
pub const CF_ID0_NAMES: u32 = 1 << 8;

// ── Message/log codes (multiplexed channel) ────────────────────────────────
/// Values 1-8 are also enum logcode values.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgCode {
    Data = 0,        // raw data
    ErrorXfer = 1,
    Info = 2,
    Error = 3,
    Warning = 4,
    ErrorSocket = 5,
    Log = 6,
    Client = 7,
    ErrorUtf8 = 8,
    Redo = 9,        // reprocess flist index
    Stats = 10,      // stats data for generator
    IoError = 22,
    IoTimeout = 33,
    Noop = 42,
    ErrorExit = 86,
    Success = 100,   // flist index successfully updated
    Deleted = 101,   // file successfully deleted
    NoSend = 102,    // sender failed to open file
}

impl MsgCode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(MsgCode::Data),
            1 => Some(MsgCode::ErrorXfer),
            2 => Some(MsgCode::Info),
            3 => Some(MsgCode::Error),
            4 => Some(MsgCode::Warning),
            5 => Some(MsgCode::ErrorSocket),
            6 => Some(MsgCode::Log),
            7 => Some(MsgCode::Client),
            8 => Some(MsgCode::ErrorUtf8),
            9 => Some(MsgCode::Redo),
            10 => Some(MsgCode::Stats),
            22 => Some(MsgCode::IoError),
            33 => Some(MsgCode::IoTimeout),
            42 => Some(MsgCode::Noop),
            86 => Some(MsgCode::ErrorExit),
            100 => Some(MsgCode::Success),
            101 => Some(MsgCode::Deleted),
            102 => Some(MsgCode::NoSend),
            _ => None,
        }
    }
}

// ── Misc sizes / limits ────────────────────────────────────────────────────
pub const MAX_ARGS: usize = 1000;
pub const MAX_BASIS_DIRS: usize = 20;
pub const MAXPATHLEN: usize = 1024;

// ── File comparison destinations ───────────────────────────────────────────
pub const COMPARE_DEST: u32 = 1;
pub const COPY_DEST: u32 = 2;
pub const LINK_DEST: u32 = 3;

// ── Flush modes ────────────────────────────────────────────────────────────
pub const NORMAL_FLUSH: u32 = 0;
pub const FULL_FLUSH: u32 = 1;
pub const MSG_FLUSH: u32 = 2;

// ── Daemon greeting prefix ────────────────────────────────────────────────
pub const RSYNCD_GREETING: &str = "@RSYNCD:";
pub const RSYNCD_OK: &str = "@RSYNCD: OK";
pub const RSYNCD_EXIT: &str = "@RSYNCD: EXIT";
pub const RSYNCD_AUTHREQD: &str = "@RSYNCD: AUTHREQD";
