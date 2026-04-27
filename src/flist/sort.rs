//! File-list sorting — mirrors `flist_sort_and_clean` / `f_name_cmp` in flist.c.

use std::cmp::Ordering;

use crate::protocol::types::{FileInfo, FileList};

/// Sort the file list in place so that protocol index `i` corresponds to
/// `flist.files[i]`.  Direct port of C rsync's `f_name_cmp`.
pub fn flist_sort(flist: &mut FileList) {
    flist.files.sort_by(file_compare);
    flist.sorted = (0..flist.files.len()).collect();
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum FncType { Item, Path }

#[derive(Copy, Clone)]
enum FncState { Dir, Slash, Base, Trailing }

/// Direct port of `f_name_cmp` from rsync's flist.c.
///
/// Walks the canonical (dirname, basename, type) representation byte-by-byte.
/// Key invariants enforced by the state machine:
///   - parent directory entries sort before their own children,
///   - regular files sort before sibling directories at the same level,
///   - protocol-version 29+ uses `t_PATH` for dirs vs `t_ITEM` for files.
pub(crate) fn file_compare(a: &FileInfo, b: &FileInfo) -> Ordering {
    let mut w1 = Walker::new(a);
    let mut w2 = Walker::new(b);
    loop {
        // Bring each side to the point where it has a byte (or is fully done).
        let r1 = w1.refill();
        let r2 = w2.refill();
        // After refilling, types may have advanced.
        if w1.t != w2.t {
            return if w1.t == FncType::Path { Ordering::Greater } else { Ordering::Less };
        }
        match (r1, r2) {
            (Some(b1), Some(b2)) => match b1.cmp(&b2) {
                Ordering::Equal => continue,
                other => return other,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

struct Walker<'a> {
    cur: &'a [u8],
    t: FncType,
    s: FncState,
    is_dir: bool,
    base: &'a [u8],
    done: bool,
}

impl<'a> Walker<'a> {
    fn new(f: &'a FileInfo) -> Self {
        let base = f.name.as_bytes();
        let is_dir = f.is_dir();
        if let Some(d) = &f.dirname {
            if !d.is_empty() {
                return Walker {
                    cur: d.as_bytes(),
                    t: FncType::Path,
                    s: FncState::Dir,
                    is_dir,
                    base,
                    done: false,
                };
            }
        }
        let (t, s, c) = if is_dir && base == b"." {
            (FncType::Item, FncState::Trailing, &b""[..])
        } else {
            (if is_dir { FncType::Path } else { FncType::Item }, FncState::Base, base)
        };
        Walker { cur: c, t, s, is_dir, base, done: false }
    }

    /// Ensure `cur` has at least one byte (advancing state machine as needed).
    /// Returns Some(byte) and consumes it, or None if the walker has no more
    /// bytes to produce.
    fn refill(&mut self) -> Option<u8> {
        if self.done { return None; }
        loop {
            if let Some((&b, rest)) = self.cur.split_first() {
                self.cur = rest;
                return Some(b);
            }
            // Advance state.
            match self.s {
                FncState::Dir => {
                    self.s = FncState::Slash;
                    self.cur = b"/";
                }
                FncState::Slash => {
                    self.t = if self.is_dir { FncType::Path } else { FncType::Item };
                    if self.t == FncType::Path && self.base == b"." {
                        self.t = FncType::Item;
                        self.s = FncState::Trailing;
                        self.cur = b"";
                    } else {
                        self.s = FncState::Base;
                        self.cur = self.base;
                    }
                }
                FncState::Base => {
                    self.s = FncState::Trailing;
                    if self.t == FncType::Path {
                        self.cur = b"/";
                    } else {
                        self.t = FncType::Item;
                        // s_TRAILING with no more bytes: walker is done.
                        self.done = true;
                        return None;
                    }
                }
                FncState::Trailing => {
                    self.t = FncType::Item;
                    self.done = true;
                    return None;
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::FileList;

    fn make_file(path: &str, is_dir: bool) -> FileInfo {
        let mode = if is_dir { 0o040755 } else { 0o100644 };
        let (dirname, name) = if let Some(p) = path.rfind('/') {
            (Some(path[..p].to_string()), path[p + 1..].to_string())
        } else {
            (None, path.to_string())
        };
        FileInfo { name, dirname, mode, ..Default::default() }
    }

    #[test]
    fn sort_basic_order() {
        let mut flist = FileList::new();
        flist.files.push(make_file("foo/bar", false));
        flist.files.push(make_file("foo", true));
        flist.files.push(make_file("aaa", false));
        flist_sort(&mut flist);

        let sorted_paths: Vec<String> = flist.sorted.iter()
            .map(|&i| flist.files[i].path())
            .collect();
        assert_eq!(sorted_paths, vec!["aaa", "foo", "foo/bar"]);
    }

    #[test]
    fn sort_files_before_sibling_dirs() {
        // C's f_name_cmp puts files before sibling directories at the same level.
        let mut flist = FileList::new();
        flist.files.push(make_file("foo0", false));
        flist.files.push(make_file("foo/child", false));
        flist.files.push(make_file("foo", true));
        flist_sort(&mut flist);

        let paths: Vec<String> = flist.sorted.iter()
            .map(|&i| flist.files[i].path())
            .collect();
        // "foo0" (file at root) sorts before "foo" (dir at root).
        // "foo" (dir) sorts before its child "foo/child".
        assert_eq!(paths, vec!["foo0", "foo", "foo/child"]);
    }

    #[test]
    fn sort_nested_tree_matches_c() {
        // Reproduces the layout used by docker regression `nested_tree` and
        // checks order matches C's f_name_cmp output.
        let mut flist = FileList::new();
        flist.files.push(make_file("Makefile", false));
        flist.files.push(make_file("docs", true));
        flist.files.push(make_file("docs/intro.md", false));
        flist.files.push(make_file("docs/api", true));
        flist.files.push(make_file("docs/api/index.md", false));
        flist.files.push(make_file("src", true));
        flist.files.push(make_file("src/main.c", false));
        flist.files.push(make_file("src/lib", true));
        flist.files.push(make_file("src/lib/util.c", false));
        flist.files.push(make_file("src/lib/util.h", false));
        flist_sort(&mut flist);
        let paths: Vec<String> = flist.sorted.iter()
            .map(|&i| flist.files[i].path()).collect();
        assert_eq!(paths, vec![
            "Makefile",
            "docs",
            "docs/intro.md",
            "docs/api",
            "docs/api/index.md",
            "src",
            "src/main.c",
            "src/lib",
            "src/lib/util.c",
            "src/lib/util.h",
        ]);
    }
}
