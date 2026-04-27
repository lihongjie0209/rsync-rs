//! Pipeline stages for the rsync protocol.
//!
//! - **Sender** (`sender.rs`): reads block checksums from the generator, computes
//!   deltas against local files, and streams tokens to the receiver.
//! - **Generator** (`generator.rs`): decides which files need updating, writes
//!   block checksums for existing local files, and signals the sender.

pub mod generator;
pub mod local;
pub mod receiver;
pub mod sender;

pub use generator::{apply_delta, Generator};
pub use local::{run_local, LocalReport};
pub use receiver::{apply_tokens, Receiver};
pub use sender::{generate_and_write_checksums, Sender};

// ── Shared helpers ─────────────────────────────────────────────────────────────

use crate::checksum::strong::ChecksumType;
use crate::protocol::constants::CsumType;

/// Map a protocol-level `CsumType` to the internal `ChecksumType` used by the
/// checksum engine.
pub(crate) fn csum_type_to_checksum_type(ct: CsumType) -> ChecksumType {
    match ct {
        CsumType::None => ChecksumType::None,
        CsumType::Md4Archaic => ChecksumType::Md4Archaic,
        CsumType::Md4Busted => ChecksumType::Md4Busted,
        CsumType::Md4Old => ChecksumType::Md4Old,
        CsumType::Md4 => ChecksumType::Md4,
        // All SHA/XXH variants fall back to MD5 (not yet implemented).
        _ => ChecksumType::Md5,
    }
}

/// Return the strong-checksum byte length for the given `CsumType`.
pub(crate) fn csum_sum_len(ct: CsumType) -> i32 {
    csum_type_to_checksum_type(ct).digest_len() as i32
}
