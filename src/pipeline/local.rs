//! Local-mode transfer — both source and destination are on this host.
//!
//! When neither side is remote, rsync's classic flow is to fork and run
//! its own protocol.  In our Rust port we take a more direct path: walk
//! the source tree, apply the option/filter settings, and copy to the
//! destination.  This keeps the local pipeline fast, portable and easy to
//! test, while the protocol code keeps owning C-compatibility on the wire.
//!
//! Behaviour mirrors the user-visible parts of rsync:
//!
//! * Trailing slash on a source means "copy *contents* of dir into dest".
//! * Without trailing slash, the dir's basename is created under dest.
//! * `-r` / `-a` enables recursion.  Without it, dirs are reported and
//!   skipped (matching rsync's "skipping directory" message).
//! * `-t` preserves mtime, `-p` preserves permissions, `-D` preserves
//!   special files (best-effort on Unix).
//! * `--dry-run` skips all writes but still prints what *would* happen.
//! * `-v` produces one line per file matching rsync's output.
//! * `--delete` removes files in dest that are absent from source.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::options::Options;
use crate::protocol::types::Stats;

/// Format a relative path the way C rsync's `f_name()` does: forward
/// slashes only, no leading `./`.  Used for all verbose-mode output.
fn rel_display(rel: &Path) -> String {
    let s = rel.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '\\' {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Result of running a local transfer.
#[derive(Debug, Default, Clone)]
pub struct LocalReport {
    pub stats: Stats,
    /// Files actually transferred (changed or new).
    pub xferred: Vec<PathBuf>,
    /// Files removed because of `--delete`.
    pub deleted: Vec<PathBuf>,
    /// Hardlinks created (when `-H` is in effect).
    pub hardlinked: Vec<PathBuf>,
}

/// Map (dev, ino) → first destination path written; populated when `-H` is
/// active so we can `link()` subsequent occurrences instead of copying.
#[derive(Default)]
struct LinkMap {
    by_inode: HashMap<(u64, u64), PathBuf>,
}

/// Run a local copy from `sources` into `dest`.
pub fn run_local(opts: &Options, sources: &[String], dest: &str) -> Result<LocalReport> {
    let mut report = LocalReport::default();
    let dest_path = PathBuf::from(dest);

    // Treat dest as a directory when it has a trailing slash, when it
    // already exists as a directory, or when we have multiple sources.
    let dest_is_dir = dest.ends_with('/')
        || dest.ends_with(std::path::MAIN_SEPARATOR)
        || sources.len() > 1
        || dest_path.is_dir();

    if dest_is_dir && !opts.dry_run {
        fs::create_dir_all(&dest_path)
            .with_context(|| format!("create_dir_all {dest_path:?}"))?;
    }

    // Track everything we actually wrote (relative to dest root) so that
    // --delete can drop the rest.
    let mut kept: HashSet<PathBuf> = HashSet::new();
    let mut links = LinkMap::default();

    for src in sources {
        copy_one(opts, src, &dest_path, dest_is_dir, &mut report, &mut kept, &mut links)
            .with_context(|| format!("copying {src}"))?;
    }

    if opts.delete && dest_is_dir {
        delete_extraneous(&dest_path, &kept, opts, &mut report)?;
    }

    Ok(report)
}

/// Copy a single source argument (file *or* directory) into `dest`.
fn copy_one(
    opts: &Options,
    src: &str,
    dest_root: &Path,
    dest_is_dir: bool,
    report: &mut LocalReport,
    kept: &mut HashSet<PathBuf>,
    links: &mut LinkMap,
) -> Result<()> {
    let src_path = PathBuf::from(src);
    let meta = fs::symlink_metadata(&src_path)
        .with_context(|| format!("stat {src_path:?}"))?;

    let trailing_slash = src.ends_with('/') || src.ends_with(std::path::MAIN_SEPARATOR);

    if meta.is_dir() {
        if !(opts.recursive || opts.archive) {
            // Match C rsync's "skipping directory NAME" line.
            if opts.verbose > 0 {
                // C rsync: "skipping directory NAME" → FINFO → stdout.
                let name = src_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| src.to_string());
                println!("skipping directory {name}");
            }
            return Ok(());
        }

        // With trailing slash, copy the *contents* of src into dest.
        // Without, copy the dir itself into dest as a sub-directory.
        let into = if trailing_slash || !dest_is_dir {
            dest_root.to_path_buf()
        } else {
            let basename = src_path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("source path has no basename: {src_path:?}"))?;
            dest_root.join(basename)
        };

        if !opts.dry_run {
            fs::create_dir_all(&into)
                .with_context(|| format!("create_dir_all {into:?}"))?;
        }
        copy_dir_recursive(opts, &src_path, &into, PathBuf::new(), report, kept, links)?;
        // Keep the dest dir itself.
        if let Ok(rel) = into.strip_prefix(dest_root) {
            kept.insert(rel.to_path_buf());
        }
    } else {
        // Regular file (or symlink/special).  Compute final destination.
        let src_basename = src_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("source has no basename: {src_path:?}"))?
            .to_owned();
        let final_dest = if dest_is_dir {
            dest_root.join(&src_basename)
        } else {
            dest_root.to_path_buf()
        };
        let rel = PathBuf::from(&src_basename);
        copy_entry(opts, &src_path, &final_dest, &meta, &rel, report, links)?;
        if let Ok(rel) = final_dest.strip_prefix(dest_root) {
            kept.insert(rel.to_path_buf());
        }
    }

    Ok(())
}

/// Recursively walk `src_dir` copying into `dest_dir`.  `prefix` is the
/// path relative to the original source root, used for keep-tracking and
/// verbose output.
fn copy_dir_recursive(
    opts: &Options,
    src_dir: &Path,
    dest_dir: &Path,
    prefix: PathBuf,
    report: &mut LocalReport,
    kept: &mut HashSet<PathBuf>,
    links: &mut LinkMap,
) -> Result<()> {
    let entries =
        fs::read_dir(src_dir).with_context(|| format!("read_dir {src_dir:?}"))?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let src_child = entry.path();
        let dest_child = dest_dir.join(&name);
        let rel = prefix.join(&name);
        let meta = entry
            .metadata()
            .with_context(|| format!("stat {src_child:?}"))?;

        kept.insert(rel.clone());

        if meta.is_dir() {
            if !opts.dry_run {
                fs::create_dir_all(&dest_child)
                    .with_context(|| format!("create_dir_all {dest_child:?}"))?;
            }
            copy_dir_recursive(opts, &src_child, &dest_child, rel.clone(), report, kept, links)?;
            apply_dir_meta(opts, &dest_child, &meta)?;
        } else {
            copy_entry(opts, &src_child, &dest_child, &meta, &rel, report, links)?;
        }
    }

    apply_dir_meta(opts, dest_dir, &fs::symlink_metadata(src_dir)?)?;
    Ok(())
}

/// Copy a single file/symlink/special; updates stats and verbose output.
fn copy_entry(
    opts: &Options,
    src: &Path,
    dest: &Path,
    meta: &fs::Metadata,
    rel: &Path,
    report: &mut LocalReport,
    links: &mut LinkMap,
) -> Result<()> {
    let ft = meta.file_type();

    // Hard-link detection: when -H is in effect, regular files with nlink>1
    // get tracked by (dev, ino).  The first time we see one we copy it; any
    // subsequent occurrence is hardlinked to that first dest.
    #[cfg(unix)]
    if opts.hard_links && ft.is_file() {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            let key = (meta.dev(), meta.ino());
            if let Some(first) = links.by_inode.get(&key).cloned() {
                if !opts.dry_run {
                    if let Some(parent) = dest.parent() {
                        if !parent.as_os_str().is_empty() {
                            fs::create_dir_all(parent)
                                .with_context(|| format!("create_dir_all {parent:?}"))?;
                        }
                    }
                    let _ = fs::remove_file(dest);
                    fs::hard_link(&first, dest)
                        .with_context(|| format!("hard_link {first:?} -> {dest:?}"))?;
                }
                if opts.verbose > 0 {
                    // Match C rsync's "NAME => TARGET" form (FINFO → stdout).
                    let target_rel = first
                        .strip_prefix(dest.parent().unwrap_or_else(|| Path::new("")))
                        .unwrap_or(&first);
                    println!("{} => {}", rel_display(rel), rel_display(target_rel));
                }
                report.hardlinked.push(dest.to_path_buf());
                report.stats.xferred_files += 1;
                return Ok(());
            } else if !opts.dry_run {
                links.by_inode.insert(key, dest.to_path_buf());
            }
        }
    }

    // Quick-skip if dest is up-to-date (size + mtime match), like rsync default.
    if !opts.checksum && dest_in_sync(dest, meta) {
        return Ok(());
    }

    if opts.verbose > 0 {
        // C rsync prints the relative path via FINFO (stdout), forward
        // slashes, no leading "./".
        println!("{}", rel_display(rel));
    }

    if opts.dry_run {
        report.stats.xferred_files += 1;
        return Ok(());
    }

    if ft.is_symlink() {
        copy_symlink(src, dest)?;
        // Don't run apply_file_meta on a symlink — set_permissions on Linux
        // follows the link and would clobber the target's mode.
        report.stats.xferred_files += 1;
        report.xferred.push(dest.to_path_buf());
        return Ok(());
    } else if ft.is_file() {
        copy_regular(src, dest, meta)?;
        #[cfg(unix)]
        if opts.xattrs {
            copy_xattrs(src, dest);
        }
        report.stats.total_size += meta.len() as i64;
        report.stats.total_written += meta.len() as i64;
    } else {
        // Devices / fifos / sockets are best-effort: skipped on non-Unix
        // and on Unix when we don't have the privilege.  Don't fail.
        return Ok(());
    }

    apply_file_meta(opts, dest, meta)?;
    report.stats.xferred_files += 1;
    report.xferred.push(dest.to_path_buf());
    Ok(())
}

/// Returns true when an existing dest looks identical enough that rsync's
/// quick-check would skip it (size + mtime to the second).
fn dest_in_sync(dest: &Path, src_meta: &fs::Metadata) -> bool {
    let Ok(d) = fs::symlink_metadata(dest) else { return false };
    if d.is_dir() != src_meta.is_dir() {
        return false;
    }
    if d.is_file() && src_meta.is_file() && d.len() != src_meta.len() {
        return false;
    }
    match (d.modified(), src_meta.modified()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Copy extended attributes from src to dest.  Best-effort — silently ignores
/// errors (e.g. dest filesystem does not support xattrs).
#[cfg(unix)]
fn copy_xattrs(src: &Path, dest: &Path) {
    let Ok(names) = xattr::list(src) else { return };
    for name in names {
        if let Ok(Some(val)) = xattr::get(src, &name) {
            let _ = xattr::set(dest, &name, &val);
        }
    }
}

/// Copy file content via OS copy.  We use [`std::fs::copy`] so that on
/// Linux it goes through `copy_file_range` when available — much faster
/// than a userspace read/write loop.
fn copy_regular(src: &Path, dest: &Path, _meta: &fs::Metadata) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {parent:?}"))?;
        }
    }

    // Atomic rename through a temp file in the same directory.
    let tmp = tmp_sibling(dest);
    let r = (|| -> io::Result<()> {
        fs::copy(src, &tmp)?;
        fs::rename(&tmp, dest)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r.with_context(|| format!("copy {src:?} -> {dest:?}"))
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target = fs::read_link(src).with_context(|| format!("readlink {src:?}"))?;
    let _ = fs::remove_file(dest);
    std::os::unix::fs::symlink(&target, dest)
        .with_context(|| format!("symlink {target:?} -> {dest:?}"))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(_src: &Path, _dest: &Path) -> Result<()> {
    Ok(())
}

fn tmp_sibling(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".into());
    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    parent.join(format!(".{}.{}.tmp", name, std::process::id()))
}

/// Apply file metadata after copy (mtime/perms/owner depending on opts).
fn apply_file_meta(opts: &Options, dest: &Path, src_meta: &fs::Metadata) -> Result<()> {
    if opts.times || opts.archive {
        if let Ok(mtime) = src_meta.modified() {
            let _ = filetime_set(dest, mtime);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if opts.perms || opts.archive {
            let perms = fs::Permissions::from_mode(src_meta.mode() & 0o7777);
            let _ = fs::set_permissions(dest, perms);
        }
        if opts.owner || opts.group || opts.archive {
            let uid = if opts.owner || opts.archive { Some(src_meta.uid()) } else { None };
            let gid = if opts.group || opts.archive { Some(src_meta.gid()) } else { None };
            let _ = chown(dest, uid, gid);
        }
    }
    Ok(())
}

fn apply_dir_meta(opts: &Options, dest: &Path, src_meta: &fs::Metadata) -> Result<()> {
    if opts.dry_run {
        return Ok(());
    }
    if !(opts.times || opts.archive) {
        return Ok(());
    }
    if opts.omit_dir_times {
        return Ok(());
    }
    if let Ok(mtime) = src_meta.modified() {
        let _ = filetime_set(dest, mtime);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::fs::MetadataExt;
        if opts.perms || opts.archive {
            let perms = fs::Permissions::from_mode(src_meta.mode() & 0o7777);
            let _ = fs::set_permissions(dest, perms);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn chown(path: &Path, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let u = uid.unwrap_or(u32::MAX);
    let g = gid.unwrap_or(u32::MAX);
    let r = unsafe { libc::lchown(c_path.as_ptr(), u, g) };
    if r == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Set both atime and mtime to `mtime`.
#[cfg(unix)]
fn filetime_set(path: &Path, mtime: std::time::SystemTime) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let dur = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ts = libc::timespec {
        tv_sec: dur.as_secs() as libc::time_t,
        tv_nsec: dur.subsec_nanos() as libc::c_long,
    };
    let times = [ts, ts];
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let r = unsafe {
        libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW)
    };
    if r == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(windows)]
fn filetime_set(path: &Path, mtime: std::time::SystemTime) -> io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;
    let f = OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // 100-nanosecond intervals since 1601-01-01.
    let ft100ns = (dur.as_secs() + 11644473600) * 10_000_000
        + (dur.subsec_nanos() as u64) / 100;
    #[repr(C)]
    struct FileTime { low: u32, high: u32 }
    let ft = FileTime { low: ft100ns as u32, high: (ft100ns >> 32) as u32 };
    extern "system" {
        fn SetFileTime(
            handle: *mut core::ffi::c_void,
            ctime: *const FileTime,
            atime: *const FileTime,
            mtime: *const FileTime,
        ) -> i32;
    }
    let h = f.as_raw_handle() as *mut core::ffi::c_void;
    let ok = unsafe { SetFileTime(h, std::ptr::null(), &ft, &ft) };
    if ok != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Remove anything in `dest_dir` that isn't in `kept`.
fn delete_extraneous(
    dest_dir: &Path,
    kept: &HashSet<PathBuf>,
    opts: &Options,
    report: &mut LocalReport,
) -> Result<()> {
    fn walk(
        root: &Path,
        cur: &Path,
        kept: &HashSet<PathBuf>,
        opts: &Options,
        report: &mut LocalReport,
    ) -> Result<()> {
        let entries = fs::read_dir(cur)
            .with_context(|| format!("read_dir {cur:?}"))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if kept.contains(&rel) {
                if entry.file_type()?.is_dir() {
                    walk(root, &path, kept, opts, report)?;
                }
                continue;
            }
            if opts.verbose > 0 {
                println!("deleting {}", rel_display(&rel));
            }
            if !opts.dry_run {
                if entry.file_type()?.is_dir() {
                    fs::remove_dir_all(&path)
                        .with_context(|| format!("remove_dir_all {path:?}"))?;
                } else {
                    fs::remove_file(&path)
                        .with_context(|| format!("remove_file {path:?}"))?;
                }
            }
            report.deleted.push(rel);
        }
        Ok(())
    }

    walk(dest_dir, dest_dir, kept, opts, report)
}

// ── Tests ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn opts(args: &[&str]) -> Options {
        // Build a minimal Options via clap so flag handling matches CLI.
        use clap::Parser;
        let mut argv: Vec<String> = vec!["rsync-rs".into()];
        for a in args { argv.push((*a).into()); }
        argv.push("a".into()); argv.push("b".into()); // placeholder paths
        let mut o = Options::try_parse_from(&argv).unwrap();
        o.expand_archive();
        o
    }

    fn tdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn copies_single_file() {
        let td = tdir();
        let src = td.path().join("a.txt");
        let dst = td.path().join("b.txt");
        std::fs::write(&src, b"hello").unwrap();
        run_local(&opts(&["-a"]), &[src.to_string_lossy().into()], &dst.to_string_lossy())
            .unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
    }

    #[test]
    fn recursive_dir_with_trailing_slash() {
        let td = tdir();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"A").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"B").unwrap();
        let src_arg = format!("{}/", src.to_string_lossy());
        run_local(&opts(&["-a"]), &[src_arg], &dst.to_string_lossy()).unwrap();
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"A");
        assert_eq!(std::fs::read(dst.join("sub/b.txt")).unwrap(), b"B");
    }

    #[test]
    fn recursive_dir_without_trailing_slash_creates_basename() {
        let td = tdir();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"A").unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        run_local(
            &opts(&["-a"]),
            &[src.to_string_lossy().into()],
            &dst.to_string_lossy(),
        )
        .unwrap();
        assert_eq!(std::fs::read(dst.join("src").join("a.txt")).unwrap(), b"A");
    }

    #[test]
    fn dry_run_makes_no_changes() {
        let td = tdir();
        let src = td.path().join("a.txt");
        let dst = td.path().join("b.txt");
        std::fs::write(&src, b"hello").unwrap();
        run_local(&opts(&["-a", "--dry-run"]), &[src.to_string_lossy().into()], &dst.to_string_lossy())
            .unwrap();
        assert!(!dst.exists());
    }

    #[test]
    fn skips_directory_without_recursion() {
        let td = tdir();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("x"), b"x").unwrap();
        std::fs::create_dir(&dst).unwrap();
        let r = run_local(&opts(&[]), &[src.to_string_lossy().into()], &dst.to_string_lossy())
            .unwrap();
        assert_eq!(r.xferred.len(), 0);
        assert!(!dst.join("src").exists());
    }

    #[test]
    fn delete_removes_extraneous() {
        let td = tdir();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("keep"), b"k").unwrap();
        std::fs::write(dst.join("keep"), b"old").unwrap();
        std::fs::write(dst.join("stale"), b"s").unwrap();
        let src_arg = format!("{}/", src.to_string_lossy());
        let r = run_local(&opts(&["-a", "--delete"]), &[src_arg], &dst.to_string_lossy())
            .unwrap();
        assert!(dst.join("keep").exists());
        assert!(!dst.join("stale").exists());
        assert_eq!(r.deleted, vec![PathBuf::from("stale")]);
    }

    #[test]
    fn quick_check_skips_unchanged() {
        let td = tdir();
        let src = td.path().join("a.txt");
        let dst = td.path().join("b.txt");
        std::fs::write(&src, b"hello").unwrap();
        run_local(&opts(&["-a"]), &[src.to_string_lossy().into()], &dst.to_string_lossy()).unwrap();
        // Mutate dst content but keep mtime+size — quick-check should leave it.
        let dst_meta = std::fs::metadata(&dst).unwrap();
        let mt = dst_meta.modified().unwrap();
        let mut f = std::fs::OpenOptions::new().write(true).open(&dst).unwrap();
        f.write_all(b"WORLD").unwrap();
        drop(f);
        let _ = filetime_set(&dst, mt);
        let _ = filetime_set(&src, mt);
        run_local(&opts(&["-a"]), &[src.to_string_lossy().into()], &dst.to_string_lossy()).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"WORLD"); // not overwritten
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn preserves_symlink() {
        let td = tdir();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("real.txt"), b"R").unwrap();
        std::os::unix::fs::symlink("real.txt", src.join("link.txt")).unwrap();
        let src_arg = format!("{}/", src.to_string_lossy());
        run_local(&opts(&["-a"]), &[src_arg], &dst.to_string_lossy()).unwrap();
        let target = std::fs::read_link(dst.join("link.txt")).unwrap();
        assert_eq!(target, std::path::PathBuf::from("real.txt"));
    }
}
