//! File I/O helpers mirroring rsync's `fileio.c`.

#![allow(dead_code)]

use std::path::Path;
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};

// ── slurp_file ────────────────────────────────────────────────────────────────

/// Read the entire contents of `path` into a `Vec<u8>`.
pub fn slurp_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("read {:?}", path))
}

// ── write_file_atomic ─────────────────────────────────────────────────────────

/// Write `data` to `path` atomically: write to a temp file in the same
/// directory then rename over the destination.
pub fn write_file_atomic(path: &Path, data: &[u8]) -> Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or(Path::new("."));
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".~rsync-tmp-{}-{}", std::process::id(), seq);
    let tmp = parent.join(&tmp_name);

    std::fs::write(&tmp, data)
        .with_context(|| format!("write tmp {:?}", tmp))?;

    if let Err(e) = std::fs::rename(&tmp, path) {
        // Clean up the temp file before propagating the error.
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename {:?} -> {:?}", tmp, path));
    }
    Ok(())
}

// ── copy_file ─────────────────────────────────────────────────────────────────

/// Copy file contents from `src` to `dst`, returning the number of bytes
/// copied.  Truncates / creates `dst`.
pub fn copy_file(src: &Path, dst: &Path) -> Result<u64> {
    let bytes = std::fs::copy(src, dst)
        .with_context(|| format!("copy {:?} -> {:?}", src, dst))?;
    Ok(bytes)
}

// ── map_file ──────────────────────────────────────────────────────────────────

/// Read up to `max_size` bytes from `path` into memory (simplified equivalent
/// of rsync's `map_file` / `mmap`).
pub fn map_file(path: &Path, max_size: u64) -> Result<Vec<u8>> {
    let mut data = slurp_file(path)?;
    if data.len() as u64 > max_size {
        data.truncate(max_size as usize);
    }
    Ok(data)
}

// ── file_checksum ─────────────────────────────────────────────────────────────

/// Compute the whole-file strong checksum.
///
/// Reads the file via [`slurp_file`] then delegates to
/// [`crate::checksum::strong::StrongChecksum::file_checksum`].
pub fn file_checksum(
    path: &Path,
    csum_type: crate::protocol::constants::CsumType,
) -> Result<Vec<u8>> {
    use crate::protocol::constants::CsumType;
    use crate::checksum::strong::{ChecksumType, StrongChecksum};

    let ct = match csum_type {
        CsumType::None => ChecksumType::None,
        CsumType::Md4Archaic => ChecksumType::Md4Archaic,
        CsumType::Md4Busted => ChecksumType::Md4Busted,
        CsumType::Md4Old => ChecksumType::Md4Old,
        CsumType::Md4 => ChecksumType::Md4,
        CsumType::Md5 => ChecksumType::Md5,
        // Unimplemented types: fall back to MD5
        CsumType::Sha1
        | CsumType::Sha256
        | CsumType::Sha512
        | CsumType::Xxh64
        | CsumType::Xxh3_64
        | CsumType::Xxh3_128 => ChecksumType::Md5,
    };

    let data = slurp_file(path)?;
    Ok(StrongChecksum::file_checksum(&data, ct))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        temp_dir().join(format!("rsync_rs_fileio_test_{}", name))
    }

    #[test]
    fn slurp_roundtrip() {
        let p = tmp_path("slurp.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let data = slurp_file(&p).unwrap();
        assert_eq!(data, b"hello world");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_file_atomic_creates_file() {
        let p = tmp_path("atomic.txt");
        let _ = std::fs::remove_file(&p);
        write_file_atomic(&p, b"atomic data").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"atomic data");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_file_atomic_overwrites() {
        let p = tmp_path("atomic_overwrite.txt");
        std::fs::write(&p, b"old").unwrap();
        write_file_atomic(&p, b"new").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn copy_file_test() {
        let src = tmp_path("copy_src.txt");
        let dst = tmp_path("copy_dst.txt");
        std::fs::write(&src, b"copy me").unwrap();
        let n = copy_file(&src, &dst).unwrap();
        assert_eq!(n, 7);
        assert_eq!(std::fs::read(&dst).unwrap(), b"copy me");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    #[test]
    fn map_file_limited() {
        let p = tmp_path("map_file.txt");
        std::fs::write(&p, b"0123456789").unwrap();
        let data = map_file(&p, 5).unwrap();
        assert_eq!(data, b"01234");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn file_checksum_md5() {
        use crate::protocol::constants::CsumType;
        let p = tmp_path("checksum.txt");
        std::fs::write(&p, b"hello").unwrap();
        let sum = file_checksum(&p, CsumType::Md5).unwrap();
        assert_eq!(sum.len(), 16);
        let _ = std::fs::remove_file(&p);
    }
}
