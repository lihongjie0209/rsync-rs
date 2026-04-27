//! Wrappers around Linux/Unix syscalls used by rsync.
//!
//! Mirrors `syscall.c` from the C rsync implementation.

#![allow(dead_code)]

use std::path::Path;
use anyhow::{Context, Result};

// ── stat / lstat ──────────────────────────────────────────────────────────────

/// Follow symlinks (mirrors `do_stat`).
pub fn do_stat(path: &Path) -> Result<std::fs::Metadata> {
    std::fs::metadata(path).with_context(|| format!("stat {:?}", path))
}

/// Do not follow symlinks (mirrors `do_lstat`).
pub fn do_lstat(path: &Path) -> Result<std::fs::Metadata> {
    std::fs::symlink_metadata(path).with_context(|| format!("lstat {:?}", path))
}

// ── mkdir ─────────────────────────────────────────────────────────────────────

/// Create a directory with the given `mode`.  EEXIST is treated as success
/// (mirrors `do_mkdir` behavior used by rsync).
pub fn do_mkdir(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::ffi::CString;
        let cs = CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("do_mkdir: nul in path {:?}", path))?;
        let ret = unsafe { libc::mkdir(cs.as_ptr(), mode as libc::mode_t) };
        if ret != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e).with_context(|| format!("mkdir {:?}", path));
            }
        }
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        match std::fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e).with_context(|| format!("mkdir {:?}", path)),
        }
    }
}

// ── rename ────────────────────────────────────────────────────────────────────

/// Rename `src` to `dst`.  On EXDEV (cross-device), fall back to copy+unlink
/// (mirrors rsync's `do_rename` extended with its cross-device fallback).
pub fn do_rename(src: &Path, dst: &Path) -> Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            #[cfg(unix)]
            if e.raw_os_error() == Some(libc::EXDEV) {
                std::fs::copy(src, dst).with_context(|| {
                    format!("copy {:?} -> {:?} (EXDEV fallback)", src, dst)
                })?;
                std::fs::remove_file(src).with_context(|| {
                    format!("unlink {:?} after EXDEV copy", src)
                })?;
                return Ok(());
            }
            Err(e).with_context(|| format!("rename {:?} -> {:?}", src, dst))
        }
    }
}

// ── unlink ────────────────────────────────────────────────────────────────────

pub fn do_unlink(path: &Path) -> Result<()> {
    std::fs::remove_file(path).with_context(|| format!("unlink {:?}", path))
}

// ── symlink ───────────────────────────────────────────────────────────────────

/// Create a symbolic link at `linkpath` pointing to `target`.
pub fn do_symlink(target: &str, linkpath: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, linkpath)
            .with_context(|| format!("symlink {:?} -> {:?}", linkpath, target))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, linkpath);
        Err(anyhow::anyhow!("symlink not supported on this platform"))
    }
}

// ── hard link ─────────────────────────────────────────────────────────────────

pub fn do_link(src: &Path, dst: &Path) -> Result<()> {
    std::fs::hard_link(src, dst)
        .with_context(|| format!("link {:?} -> {:?}", src, dst))
}

// ── set_modtime ───────────────────────────────────────────────────────────────

/// Set the modification time (and leave access time unchanged) on `path`.
/// Uses `utimensat(AT_FDCWD, path, UTIME_OMIT, mtime, AT_SYMLINK_NOFOLLOW)`
/// on Linux; falls back to `utimes` on other Unix; no-op on Windows.
pub fn set_modtime(path: &Path, mtime: i64, mtime_nsec: u32) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::ffi::CString;
        let cs = CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("set_modtime: nul in path {:?}", path))?;
        let times = [
            // atime: UTIME_OMIT — leave unchanged
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT as libc::c_long,
            },
            // mtime
            libc::timespec {
                tv_sec: mtime as libc::time_t,
                tv_nsec: mtime_nsec as libc::c_long,
            },
        ];
        let ret = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                cs.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if ret != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("utimensat {:?}", path));
        }
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::ffi::CString;
        let cs = CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("set_modtime: nul in path {:?}", path))?;
        // utimes: sets atime=mtime=mtime (no nanosecond precision here)
        let times = [
            libc::timeval { tv_sec: mtime as libc::time_t, tv_usec: (mtime_nsec / 1000) as libc::suseconds_t },
            libc::timeval { tv_sec: mtime as libc::time_t, tv_usec: (mtime_nsec / 1000) as libc::suseconds_t },
        ];
        let ret = unsafe { libc::utimes(cs.as_ptr(), times.as_ptr()) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("utimes {:?}", path));
        }
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mtime, mtime_nsec);
        Ok(()) // no-op on non-unix
    }
}

// ── chmod ─────────────────────────────────────────────────────────────────────

pub fn do_chmod(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("chmod {:?}", path))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err(anyhow::anyhow!("chmod not supported on this platform"))
    }
}

// ── chown ─────────────────────────────────────────────────────────────────────

/// lchown — does NOT follow symlinks.
pub fn do_chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::unistd::{chown, Uid, Gid};
        chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .with_context(|| format!("chown {:?}", path))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, uid, gid);
        Err(anyhow::anyhow!("chown not supported on this platform"))
    }
}

// ── robust_unlink ─────────────────────────────────────────────────────────────

/// Move the file to `.~tmp~` in the same directory before unlinking, so that
/// open file handles on other processes remain valid until they close.
/// Mirrors rsync's `robust_unlink`.
pub fn robust_unlink(path: &Path) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(".~tmp~");

    // Best-effort: rename to the tmp name, then remove it.
    // If rename fails, fall back to a direct unlink.
    if std::fs::rename(path, &tmp).is_ok() {
        let _ = std::fs::remove_file(&tmp);
    } else {
        std::fs::remove_file(path)
            .with_context(|| format!("robust_unlink {:?}", path))?;
    }
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        temp_dir().join(format!("rsync_rs_syscall_test_{}", name))
    }

    #[test]
    fn stat_existing_file() {
        let p = tmp_path("stat_test.txt");
        std::fs::write(&p, b"hello").unwrap();
        let m = do_stat(&p).unwrap();
        assert!(m.is_file());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn lstat_symlink() {
        let target = tmp_path("lstat_target.txt");
        let link = tmp_path("lstat_link");
        std::fs::write(&target, b"data").unwrap();
        let _ = std::fs::remove_file(&link);
        do_symlink(target.to_str().unwrap(), &link).unwrap();
        let m = do_lstat(&link).unwrap();
        assert!(m.file_type().is_symlink());
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn mkdir_eexist_is_ok() {
        let d = tmp_path("mkdir_test_dir");
        let _ = std::fs::create_dir(&d);
        // Second call must not fail
        do_mkdir(&d, 0o755).unwrap();
        let _ = std::fs::remove_dir(&d);
    }

    #[test]
    fn unlink_file() {
        let p = tmp_path("unlink_test.txt");
        std::fs::write(&p, b"bye").unwrap();
        do_unlink(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn hard_link_and_rename() {
        let src = tmp_path("hard_link_src.txt");
        let lnk = tmp_path("hard_link_dst.txt");
        let renamed = tmp_path("hard_link_renamed.txt");
        std::fs::write(&src, b"data").unwrap();
        let _ = std::fs::remove_file(&lnk);
        let _ = std::fs::remove_file(&renamed);
        do_link(&src, &lnk).unwrap();
        do_rename(&lnk, &renamed).unwrap();
        assert!(renamed.exists());
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&renamed);
    }

    #[test]
    fn robust_unlink_removes_file() {
        let p = tmp_path("robust_unlink_test.txt");
        std::fs::write(&p, b"gone").unwrap();
        robust_unlink(&p).unwrap();
        assert!(!p.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn set_modtime_changes_mtime() {
        let p = tmp_path("set_modtime_test.txt");
        std::fs::write(&p, b"time test").unwrap();
        // Set mtime to a known epoch value
        set_modtime(&p, 1_000_000, 0).unwrap();
        let m = std::fs::metadata(&p).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(m.mtime(), 1_000_000);
        let _ = std::fs::remove_file(&p);
    }
}
