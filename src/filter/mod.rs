#![allow(dead_code)]

use anyhow::{anyhow, Result};
use std::fs;

// ---------------------------------------------------------------------------
// FILTRULE_* flag constants (mirrors exclude.c)
// ---------------------------------------------------------------------------

pub const FILTRULE_WILD: u32 = 1 << 0;
pub const FILTRULE_WILD2: u32 = 1 << 1;        // "**" present
pub const FILTRULE_WILD2_PREFIX: u32 = 1 << 2; // pattern starts with "**"
pub const FILTRULE_WILD3_SUFFIX: u32 = 1 << 3; // not used in Phase 1
pub const FILTRULE_ABS_PATH: u32 = 1 << 4;     // pattern rooted with '/'
pub const FILTRULE_INCLUDE: u32 = 1 << 5;      // include rule (else exclude)
pub const FILTRULE_DIRECTORY: u32 = 1 << 6;    // pattern ends with '/'
pub const FILTRULE_NO_PREFIXES: u32 = 1 << 9;
pub const FILTRULE_PERISHABLE: u32 = 1 << 19;
pub const FILTRULE_CLEAR_LIST: u32 = 1 << 18;
pub const FILTRULE_SENDER_SIDE: u32 = 1 << 16;
pub const FILTRULE_RECEIVER_SIDE: u32 = 1 << 17;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FilterRule {
    pub pattern: String,
    pub rflags: u32,
    pub slash_cnt: u32,
}

#[derive(Debug, Clone, Default)]
pub struct FilterList {
    pub rules: Vec<FilterRule>,
}

// ---------------------------------------------------------------------------
// FilterList impl
// ---------------------------------------------------------------------------

impl FilterList {
    pub fn new() -> Self {
        FilterList { rules: Vec::new() }
    }

    /// Add an exclude pattern.
    pub fn add_exclude(&mut self, pattern: &str) {
        self.rules.push(build_rule(pattern, false));
    }

    /// Add an include pattern.
    pub fn add_include(&mut self, pattern: &str) {
        self.rules.push(build_rule(pattern, true));
    }

    /// Parse a filter rule string.
    ///
    /// Recognised prefixes:
    /// - `"-"` / `"exclude"` — exclude rule
    /// - `"+"` / `"include"` — include rule
    /// - `"!"` — clear list
    /// - `"merge"` / `"."` / `":"` — merge-file (not implemented; silently skipped)
    pub fn parse_rule(&mut self, rule_str: &str) {
        let s = rule_str.trim();
        if s.is_empty() || s.starts_with('#') {
            return;
        }

        // Try two-char prefix first ("- ", "+ ")
        let (prefix, rest) = if s.len() >= 2 && (s.as_bytes()[1] == b' ' || s.as_bytes()[1] == b',') {
            (&s[..1], s[2..].trim())
        } else {
            // Try word prefix ("exclude ", "include ", "merge ", etc.)
            let mut parts = s.splitn(2, ' ');
            let word = parts.next().unwrap_or("");
            let tail = parts.next().unwrap_or("").trim();
            (word, tail)
        };

        match prefix {
            "-" | "exclude" => self.add_exclude(rest),
            "+" | "include" => self.add_include(rest),
            "!" => {
                // Clear-list pseudo-rule
                self.rules.push(FilterRule {
                    pattern: String::new(),
                    rflags: FILTRULE_CLEAR_LIST,
                    slash_cnt: 0,
                });
            }
            // merge / dir-merge / "." / ":" — not implemented in Phase 1
            _ => {}
        }
    }

    /// Load rules from a file (one per line; lines starting with '#' are comments).
    pub fn load_from_file(&mut self, path: &str) -> Result<()> {
        let content = fs::read_to_string(path)
            .map_err(|e| anyhow!("cannot read filter file '{}': {}", path, e))?;
        for line in content.lines() {
            self.parse_rule(line);
        }
        Ok(())
    }

    /// Returns `true` if the file should be excluded from the transfer.
    ///
    /// Rules are evaluated in order; the first match wins.  
    /// A CLEAR_LIST rule resets all rules seen so far (from the perspective of
    /// remaining rules).
    pub fn is_excluded(&self, name: &str, is_dir: bool) -> bool {
        for rule in &self.rules {
            if rule.rflags & FILTRULE_CLEAR_LIST != 0 {
                // A clear rule never matches by itself; it was already handled
                // when building the list.  In a fully correct implementation
                // the list would be split at clear points; for Phase 1 we just
                // skip the marker.
                continue;
            }
            if rule.matches(name, is_dir) {
                // INCLUDE rule → do NOT exclude
                return rule.rflags & FILTRULE_INCLUDE == 0;
            }
        }
        false // no rule matched → include
    }

    /// Build a FilterList from parsed Options.
    pub fn from_options(opts: &crate::options::Options) -> Result<Self> {
        let mut list = FilterList::new();

        // --filter rules (highest precedence, in order given)
        for rule in &opts.filter {
            list.parse_rule(rule);
        }

        // --include-from files
        for path in &opts.include_from {
            list.load_from_file(path)?;
        }

        // --exclude-from files
        for path in &opts.exclude_from {
            list.load_from_file(path)?;
        }

        // Inline --include patterns
        for pat in &opts.include_patterns {
            list.add_include(pat);
        }

        // Inline --exclude patterns
        for pat in &opts.exclude {
            list.add_exclude(pat);
        }

        // CVS default excludes
        if opts.cvs_exclude {
            for pat in CVS_EXCLUDES {
                list.add_exclude(pat);
            }
        }

        Ok(list)
    }
}

// ---------------------------------------------------------------------------
// FilterRule impl
// ---------------------------------------------------------------------------

impl FilterRule {
    fn matches(&self, name: &str, is_dir: bool) -> bool {
        // DIRECTORY rules only match directories
        if self.rflags & FILTRULE_DIRECTORY != 0 && !is_dir {
            return false;
        }

        let pattern = &self.pattern;

        // If pattern contains a slash (other than a trailing one from DIRECTORY
        // detection), match against the full path component; otherwise match
        // only the last path component (basename).
        let match_target: &str = if self.slash_cnt > 0 || self.rflags & FILTRULE_ABS_PATH != 0 {
            name
        } else {
            // Use basename
            name.rsplit('/').next().unwrap_or(name)
        };

        if self.rflags & FILTRULE_WILD != 0 {
            wildmatch(pattern, match_target)
        } else {
            // Plain string comparison
            if self.rflags & FILTRULE_ABS_PATH != 0 {
                match_target == pattern.trim_start_matches('/')
            } else {
                match_target == pattern
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule construction helper
// ---------------------------------------------------------------------------

fn build_rule(pattern: &str, is_include: bool) -> FilterRule {
    let mut rflags: u32 = 0;
    if is_include {
        rflags |= FILTRULE_INCLUDE;
    }

    // Strip trailing '/' — marks as directory-only rule
    let (pat, dir_only) = if pattern.ends_with('/') {
        (&pattern[..pattern.len() - 1], true)
    } else {
        (pattern, false)
    };

    if dir_only {
        rflags |= FILTRULE_DIRECTORY;
    }

    // Absolute path pattern
    if pat.starts_with('/') {
        rflags |= FILTRULE_ABS_PATH;
    }

    // Detect wildcards
    let has_wild = pat.contains('*') || pat.contains('?') || pat.contains('[');
    if has_wild {
        rflags |= FILTRULE_WILD;
    }
    if pat.contains("**") {
        rflags |= FILTRULE_WILD2;
        if pat.starts_with("**") {
            rflags |= FILTRULE_WILD2_PREFIX;
        }
    }

    // Count slashes (excluding leading one) to decide path-level matching
    let slash_cnt = pat
        .trim_start_matches('/')
        .chars()
        .filter(|&c| c == '/')
        .count() as u32;

    // Strip leading slash from stored pattern so comparisons are simpler
    let stored = pat.trim_start_matches('/').to_string();

    FilterRule {
        pattern: stored,
        rflags,
        slash_cnt,
    }
}

// ---------------------------------------------------------------------------
// Wildcard matching (rsync-compatible)
// Supports: *, **, ?, [...]
// ---------------------------------------------------------------------------

/// Match `text` against rsync-style `pattern`.
///
/// - `?`  matches any single character (not `/`)
/// - `*`  matches any sequence of characters except `/`
/// - `**` matches any sequence of characters including `/`
/// - `[chars]` character class (like shell globs)
fn wildmatch(pattern: &str, text: &str) -> bool {
    wildmatch_bytes(pattern.as_bytes(), text.as_bytes())
}

fn wildmatch_bytes(pat: &[u8], text: &[u8]) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;

    loop {
        if pi == pat.len() {
            return ti == text.len();
        }

        if pat[pi] == b'*' {
            // Consume all leading stars, noting if any double-star is present
            let mut double = false;
            let start = pi;
            while pi < pat.len() && pat[pi] == b'*' {
                if pi + 1 < pat.len() && pat[pi + 1] == b'*' {
                    double = true;
                }
                pi += 1;
                if double && pi < pat.len() && pat[pi] == b'*' {
                    // still in a run
                } else if double {
                    break;
                }
            }
            // Re-detect: any `**` in the consumed run?
            double = pat[start..pi].windows(2).any(|w| w == b"**");

            // Skip any remaining stars in this run
            while pi < pat.len() && pat[pi] == b'*' {
                double = true;
                pi += 1;
            }

            let rest_pat = &pat[pi..];

            if rest_pat.is_empty() {
                // '*' at end only matches if remaining text has no '/'
                if double {
                    return true; // '**' at end matches everything
                } else {
                    return !text[ti..].contains(&b'/');
                }
            }

            // Try matching rest_pat at every valid position
            let mut i = ti;
            loop {
                if wildmatch_bytes(rest_pat, &text[i..]) {
                    return true;
                }
                if i == text.len() {
                    break;
                }
                // Single '*' cannot skip past '/'
                if !double && text[i] == b'/' {
                    break;
                }
                i += 1;
            }
            return false;
        }

        if ti == text.len() {
            return false;
        }

        if pat[pi] == b'[' {
            let (matched, consumed) = match_char_class(&pat[pi..], text[ti]);
            if !matched {
                return false;
            }
            pi += consumed;
            ti += 1;
        } else if pat[pi] == b'?' {
            if text[ti] == b'/' {
                return false;
            }
            pi += 1;
            ti += 1;
        } else {
            if pat[pi] != text[ti] {
                return false;
            }
            pi += 1;
            ti += 1;
        }
    }
}

/// Parse a `[...]` character class starting at `pat[0]`.
/// Returns (matched, bytes_consumed_from_pat).
fn match_char_class(pat: &[u8], ch: u8) -> (bool, usize) {
    // pat[0] == b'['
    let mut i = 1usize;
    let negate = i < pat.len() && pat[i] == b'!';
    if negate {
        i += 1;
    }

    let mut matched = false;
    let mut first = true;

    while i < pat.len() {
        if pat[i] == b']' && !first {
            i += 1; // consume ']'
            break;
        }
        first = false;

        // Range: a-z
        if i + 2 < pat.len() && pat[i + 1] == b'-' && pat[i + 2] != b']' {
            if ch >= pat[i] && ch <= pat[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if ch == pat[i] {
                matched = true;
            }
            i += 1;
        }
    }

    let result = if negate { !matched } else { matched };
    (result, i)
}

// ---------------------------------------------------------------------------
// Default CVS excludes (--cvs-exclude)
// ---------------------------------------------------------------------------

const CVS_EXCLUDES: &[&str] = &[
    "RCS",
    "SCCS",
    "CVS",
    "CVS.adm",
    "RCSLOG",
    "cvslog.*",
    "tags",
    "TAGS",
    ".make.state",
    ".nse_depinfo",
    "*~",
    "#*",
    ".#*",
    ",*",
    "_$*",
    "*$",
    "*.old",
    "*.bak",
    "*.BAK",
    "*.orig",
    "*.rej",
    ".del-*",
    "*.a",
    "*.olb",
    "*.o",
    "*.obj",
    "*.so",
    "*.exe",
    "*.Z",
    "*.elc",
    "*.ln",
    "core",
    ".svn/",
    ".git/",
    ".hg/",
    ".bzr/",
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wildmatch_star() {
        assert!(wildmatch("*.rs", "main.rs"));
        assert!(wildmatch("*.rs", "foo.rs"));
        assert!(!wildmatch("*.rs", "foo.go"));
        assert!(!wildmatch("*.rs", "src/main.rs")); // '*' should not cross '/'
    }

    #[test]
    fn test_wildmatch_double_star() {
        assert!(wildmatch("**/*.rs", "src/main.rs"));
        assert!(wildmatch("**/*.rs", "a/b/c/foo.rs"));
        assert!(!wildmatch("**/*.rs", "foo.go"));
    }

    #[test]
    fn test_wildmatch_question() {
        assert!(wildmatch("fo?.rs", "foo.rs"));
        assert!(!wildmatch("fo?.rs", "fo/.rs"));
    }

    #[test]
    fn test_wildmatch_char_class() {
        assert!(wildmatch("[abc].rs", "a.rs"));
        assert!(wildmatch("[abc].rs", "b.rs"));
        assert!(!wildmatch("[abc].rs", "d.rs"));
        assert!(wildmatch("[a-z].rs", "m.rs"));
        assert!(!wildmatch("[a-z].rs", "M.rs"));
    }

    #[test]
    fn test_exclude_include_order() {
        let mut list = FilterList::new();
        list.add_exclude("*.log");
        list.add_include("important.log");
        // exclude wins because it comes first
        assert!(list.is_excluded("debug.log", false));
        assert!(list.is_excluded("important.log", false)); // exclude matched first
    }

    #[test]
    fn test_include_before_exclude() {
        let mut list = FilterList::new();
        list.add_include("important.log");
        list.add_exclude("*.log");
        assert!(!list.is_excluded("important.log", false));
        assert!(list.is_excluded("debug.log", false));
    }

    #[test]
    fn test_directory_rule() {
        let mut list = FilterList::new();
        list.add_exclude(".git/");
        assert!(list.is_excluded(".git", true));
        assert!(!list.is_excluded(".git", false)); // not a dir
    }

    #[test]
    fn test_parse_rule_minus() {
        let mut list = FilterList::new();
        list.parse_rule("- *.o");
        assert!(list.is_excluded("foo.o", false));
        assert!(!list.is_excluded("foo.c", false));
    }

    #[test]
    fn test_parse_rule_plus() {
        let mut list = FilterList::new();
        list.parse_rule("+ keep.log");
        list.parse_rule("- *.log");
        assert!(!list.is_excluded("keep.log", false));
        assert!(list.is_excluded("drop.log", false));
    }
}
