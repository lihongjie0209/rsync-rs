//! Server-side parser for the bundled C-style flag string sent by an
//! rsync client over `--server`.
//!
//! When a client invokes the server it bundles short flags into a single
//! token like `-vlogDtpre.iLsfxCIvu` (see `options.c::server_options()` in
//! the C source). On the wire that token is positional; the *server*
//! process must decode it to recover the negotiation-relevant booleans
//! (which files have which xfer flags, what id lists to read, etc.).
//!
//! The encoding rules we need to mirror exactly:
//!
//! * `-a` is *not* expanded by the client into `-rlptgoD`. The client
//!   pushes the literal letter `a` and the server is expected to expand
//!   it itself (matching popt's `OPT_ARCHIVE` handler in `options.c`).
//! * `-D` enables both `--devices` and `--specials`.
//! * `-e<chars>` carries protocol-extension feature flags (`e.iLsf` etc.)
//!   and continues past `.` until the end of the token. We never need to
//!   interpret those characters here, but we *must* not let them leak
//!   into the boolean flag parsing.
//! * Long options sent as separate tokens (e.g. `--numeric-ids`) are
//!   handled outside this parser.
//!
//! `parse_server_flags` returns a [`ServerFlags`] which the caller
//! pretty-prints under `--debug` and feeds into `flist::Preserve`.

use std::fmt;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerFlags {
    pub verbose: u32,
    pub quiet: bool,
    pub archive: bool,
    pub recursive: bool,
    pub links: bool,
    pub perms: bool,
    pub times: bool,
    pub group: bool,
    pub owner: bool,
    pub devices: bool,
    pub specials: bool,
    pub checksum: bool,
    pub update: bool,
    pub hard_links: bool,
    pub copy_links: bool,
    pub keep_dirlinks: bool,
    pub whole_file: bool,
    pub dry_run: bool,
    pub relative: bool,
    pub itemize: bool,
    pub fuzzy: bool,
    pub xattrs: bool,
    pub cvs_exclude: bool,
    pub ignore_times: bool,
    pub one_file_system: bool,
    pub protect_args: bool,
    pub compress: bool,
    pub delete: bool,
    /// Unrecognised characters seen in the bundled token (excluding the
    /// `-e` extension tail, which is consumed silently).
    pub unknown: String,
    /// Verbatim tail after `-e`, captured for debugging.
    pub e_tail: String,
}

impl ServerFlags {
    /// Resolve the archive shortcut: `-a` → `-rlptgoD`.
    /// Idempotent; safe to call after manual flag overrides.
    pub fn expand_archive(&mut self) {
        if self.archive {
            self.recursive = true;
            self.links = true;
            self.perms = true;
            self.times = true;
            self.group = true;
            self.owner = true;
            self.devices = true;
            self.specials = true;
        }
    }

    /// Collapse to the [`flist::Preserve`] booleans the wire protocol
    /// actually consults.
    pub fn to_preserve(&self) -> crate::flist::Preserve {
        crate::flist::Preserve {
            uid: self.owner,
            gid: self.group,
            times: self.times,
            devices: self.devices,
        }
    }
}

impl fmt::Display for ServerFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ServerFlags {{ v={} a={} r={} l={} p={} t={} g={} o={} D={} q={} c={} u={} H={} L={} k={} W={} n={} R={} i={} f={} x={} C={} I={} unknown={:?} e_tail={:?} }}",
            self.verbose,
            self.archive,
            self.recursive,
            self.links,
            self.perms,
            self.times,
            self.group,
            self.owner,
            self.devices,
            self.quiet,
            self.checksum,
            self.update,
            self.hard_links,
            self.copy_links,
            self.keep_dirlinks,
            self.whole_file,
            self.dry_run,
            self.relative,
            self.itemize,
            self.fuzzy,
            self.xattrs,
            self.cvs_exclude,
            self.ignore_times,
            self.unknown,
            self.e_tail,
        )
    }
}

/// Parse a bundled C-style flag token (e.g. `-vlogDtpre.iLsfxCIvu`).
///
/// The leading `-` is required (returns empty result without it).
/// The function is total: unknown characters accumulate in
/// [`ServerFlags::unknown`] rather than aborting.
pub fn parse_server_flags(arg: &str) -> ServerFlags {
    let mut out = ServerFlags::default();
    let bytes = arg.as_bytes();
    if bytes.first() != Some(&b'-') {
        return out;
    }
    let mut i = 1usize;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        match ch {
            'v' => out.verbose += 1,
            'q' => out.quiet = true,
            'a' => out.archive = true,
            'r' => out.recursive = true,
            'l' => out.links = true,
            'p' => out.perms = true,
            't' => out.times = true,
            'g' => out.group = true,
            'o' => out.owner = true,
            'D' => {
                out.devices = true;
                out.specials = true;
            }
            'c' => out.checksum = true,
            'u' => out.update = true,
            'H' => out.hard_links = true,
            'L' => out.copy_links = true,
            'k' => out.keep_dirlinks = true,
            'W' => out.whole_file = true,
            'n' => out.dry_run = true,
            'R' => out.relative = true,
            'i' => out.itemize = true,
            'f' => out.fuzzy = true,
            'x' => {
                // First 'x' = --one-file-system, second 'x' = --xattrs (rsync
                // packs them positionally). For our purposes treat any 'x'
                // as enabling both — neither affects flist negotiation.
                if out.one_file_system {
                    out.xattrs = true;
                } else {
                    out.one_file_system = true;
                }
            }
            'C' => out.cvs_exclude = true,
            'I' => out.ignore_times = true,
            's' => out.protect_args = true,
            'z' => out.compress = true,
            'e' => {
                // Protocol-extension tail: everything after `e` (including
                // the `.` separator and feature letters) is opaque.
                out.e_tail = arg[i + 1..].to_string();
                break;
            }
            other => out.unknown.push(other),
        }
        i += 1;
    }
    out.expand_archive();
    out
}

/// Walk *all* args from the server CLI tail and merge every bundled
/// flag token. Long options (`--numeric-ids` etc.) are ignored; positional
/// path arguments are returned separately.
pub fn parse_server_argv<'a, I, S>(args: I) -> (ServerFlags, Vec<&'a str>)
where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + 'a + ?Sized,
{
    let mut flags = ServerFlags::default();
    let mut paths: Vec<&str> = Vec::new();
    for raw in args {
        let s: &str = raw.as_ref();
        if let Some(rest) = s.strip_prefix("--") {
            // Long options: only ones we care about for the wire.
            match rest {
                "numeric-ids" => { /* doesn't affect preserve booleans */ }
                "delete" | "delete-before" | "delete-during" | "delete-after" => {
                    flags.delete = true;
                }
                _ => {}
            }
        } else if s.starts_with('-') && s.len() > 1 {
            let f = parse_server_flags(s);
            flags.verbose += f.verbose;
            flags.quiet |= f.quiet;
            flags.archive |= f.archive;
            flags.recursive |= f.recursive;
            flags.links |= f.links;
            flags.perms |= f.perms;
            flags.times |= f.times;
            flags.group |= f.group;
            flags.owner |= f.owner;
            flags.devices |= f.devices;
            flags.specials |= f.specials;
            flags.checksum |= f.checksum;
            flags.update |= f.update;
            flags.hard_links |= f.hard_links;
            flags.copy_links |= f.copy_links;
            flags.keep_dirlinks |= f.keep_dirlinks;
            flags.whole_file |= f.whole_file;
            flags.dry_run |= f.dry_run;
            flags.relative |= f.relative;
            flags.itemize |= f.itemize;
            flags.fuzzy |= f.fuzzy;
            flags.xattrs |= f.xattrs;
            flags.cvs_exclude |= f.cvs_exclude;
            flags.ignore_times |= f.ignore_times;
            flags.one_file_system |= f.one_file_system;
            flags.protect_args |= f.protect_args;
            flags.compress |= f.compress;
            if !f.e_tail.is_empty() {
                flags.e_tail = f.e_tail;
            }
            for c in f.unknown.chars() {
                if !flags.unknown.contains(c) {
                    flags.unknown.push(c);
                }
            }
        } else {
            paths.push(s);
        }
    }
    flags.expand_archive();
    (flags, paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_default() {
        assert_eq!(parse_server_flags(""), ServerFlags::default());
        assert_eq!(parse_server_flags("foo"), ServerFlags::default());
    }

    #[test]
    fn archive_alone_expands_to_rlptgoD() {
        let f = parse_server_flags("-a");
        assert!(f.archive);
        assert!(f.recursive);
        assert!(f.links);
        assert!(f.perms);
        assert!(f.times);
        assert!(f.group);
        assert!(f.owner);
        assert!(f.devices);
        assert!(f.specials);
    }

    #[test]
    fn av_token_sets_archive_and_verbose() {
        // Real client emits `-a` separately, but `-av` collapses both.
        let f = parse_server_flags("-av");
        assert_eq!(f.verbose, 1);
        assert!(f.archive);
        assert!(f.owner && f.group && f.times && f.devices);
    }

    #[test]
    fn explicit_letters_match_archive_expansion() {
        let f = parse_server_flags("-rlptgoD");
        assert!(f.recursive && f.links && f.perms && f.times);
        assert!(f.group && f.owner && f.devices && f.specials);
        assert!(!f.archive);
    }

    #[test]
    fn vrt_token_keeps_owner_group_off() {
        // Used by --verify/restore-time-only style tests in our regress
        // suite. Must NOT pretend to preserve uid/gid.
        let f = parse_server_flags("-vrt");
        assert_eq!(f.verbose, 1);
        assert!(f.recursive);
        assert!(f.times);
        assert!(!f.owner);
        assert!(!f.group);
        assert!(!f.devices);
    }

    #[test]
    fn extension_tail_is_consumed_after_e() {
        // Real client bundle from a recent capture.
        let f = parse_server_flags("-vlogDtpre.iLsfxCIvu");
        assert_eq!(f.verbose, 1, "single leading 'v'");
        assert!(f.links && f.owner && f.group && f.devices);
        assert!(f.times && f.perms && f.recursive);
        assert_eq!(f.e_tail, ".iLsfxCIvu");
        // Letters after `e` must NOT bleed into top-level flags:
        assert!(!f.itemize, "i is in e-tail, not a top-level flag");
    }

    #[test]
    fn unknown_chars_are_collected_not_dropped() {
        let f = parse_server_flags("-aZ?");
        assert!(f.archive);
        assert_eq!(f.unknown, "Z?");
    }

    #[test]
    fn preserve_projection_matches_owner_group_times_devices() {
        let f = parse_server_flags("-a");
        let p = f.to_preserve();
        assert!(p.uid && p.gid && p.times && p.devices);

        let f = parse_server_flags("-vrt");
        let p = f.to_preserve();
        assert!(!p.uid && !p.gid && p.times && !p.devices);
    }

    #[test]
    fn parse_server_argv_merges_multiple_tokens() {
        let argv: Vec<String> =
            ["--server", "-vlogDtpre.iLsfxCIvu", "--numeric-ids", ".", "/tmp/dst"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let (flags, paths) = parse_server_argv(&argv);
        assert!(flags.archive == false, "archive flag itself isn't on the wire");
        assert!(flags.owner && flags.group && flags.times && flags.devices);
        assert_eq!(paths, vec![".", "/tmp/dst"]);
    }

    #[test]
    fn z_flag_sets_compress() {
        let f = parse_server_flags("-vz");
        assert!(f.compress);
        assert_eq!(f.verbose, 1);
        let f = parse_server_flags("-avz");
        assert!(f.compress && f.archive && f.verbose == 1);
    }

    #[test]
    fn parse_server_argv_handles_separate_a_token() {
        let argv = ["--server", "-a", "."].map(String::from);
        let (flags, _) = parse_server_argv(&argv);
        assert!(flags.archive);
        assert!(flags.owner && flags.group && flags.times);
    }
}
