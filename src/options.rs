#![allow(dead_code)]

use anyhow::{bail, Result};

#[derive(Debug, Clone, Default, clap::Parser)]
#[command(
    name = "rsync-rs",
    about = "A Rust rsync implementation",
    disable_help_flag = true,
    disable_version_flag = true,
)]
pub struct Options {
    // Verbosity
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    #[arg(short = 'q', long)]
    pub quiet: bool,

    // Archive mode (-a = -rlptgoD)
    #[arg(short = 'a', long)]
    pub archive: bool,

    // Recursion
    #[arg(short = 'r', long)]
    pub recursive: bool,

    // Links
    #[arg(short = 'l', long)]
    pub links: bool,
    #[arg(short = 'L', long)]
    pub copy_links: bool,
    #[arg(long)]
    pub copy_dirlinks: bool,
    #[arg(short = 'k', long)]
    pub keep_dirlinks: bool,

    // Permissions
    #[arg(short = 'p', long)]
    pub perms: bool,
    #[arg(short = 'E', long)]
    pub executability: bool,
    #[arg(short = 'A', long)]
    pub acls: bool,
    #[arg(short = 'X', long)]
    pub xattrs: bool,

    // Owner/group
    #[arg(short = 'o', long)]
    pub owner: bool,
    #[arg(short = 'g', long)]
    pub group: bool,
    #[arg(long)]
    pub numeric_ids: bool,

    // Device files
    #[arg(short = 'D', long)]
    pub devices: bool,
    #[arg(long)]
    pub specials: bool,

    // Times
    #[arg(short = 't', long)]
    pub times: bool,
    #[arg(short = 'O', long)]
    pub omit_dir_times: bool,
    #[arg(long)]
    pub omit_link_times: bool,

    // Modification detection
    #[arg(short = 'c', long)]
    pub checksum: bool,
    #[arg(short = 'u', long)]
    pub update: bool,
    #[arg(short = 'i', long)]
    pub itemize_changes: bool,

    // Deletion
    #[arg(long)]
    pub delete: bool,
    #[arg(long)]
    pub delete_before: bool,
    #[arg(long)]
    pub delete_during: bool,
    #[arg(long)]
    pub delete_after: bool,
    #[arg(long)]
    pub delete_excluded: bool,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub max_delete: Option<i32>,

    // File selection
    #[arg(long)]
    pub exclude: Vec<String>,
    #[arg(long)]
    pub exclude_from: Vec<String>,
    #[arg(long, name = "include")]
    pub include_patterns: Vec<String>,
    #[arg(long)]
    pub include_from: Vec<String>,
    #[arg(short = 'F', long = "filter", action = clap::ArgAction::Append)]
    pub filter: Vec<String>,
    #[arg(long)]
    pub cvs_exclude: bool,

    // Compression
    #[arg(short = 'z', long)]
    pub compress: bool,
    #[arg(long)]
    pub compress_level: Option<i32>,

    // Transfer control
    #[arg(short = 'W', long)]
    pub whole_file: bool,
    #[arg(long)]
    pub no_whole_file: bool,
    #[arg(long)]
    pub append: bool,
    #[arg(long)]
    pub inplace: bool,
    #[arg(long)]
    pub partial: bool,
    #[arg(long)]
    pub partial_dir: Option<String>,

    // Remote shell
    #[arg(short = 'e', long, value_name = "COMMAND")]
    pub rsh: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub rsync_path: Option<String>,

    // Daemon mode (server-side)
    #[arg(long, hide = true)]
    pub server: bool,
    #[arg(long, hide = true)]
    pub sender: bool,
    #[arg(long)]
    pub daemon: bool,
    #[arg(long, value_name = "FILE")]
    pub config: Option<String>,
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    #[arg(long)]
    pub no_detach: bool,

    // Hard links
    #[arg(short = 'H', long)]
    pub hard_links: bool,

    // Backup
    #[arg(short = 'b', long)]
    pub backup: bool,
    #[arg(long)]
    pub backup_dir: Option<String>,
    #[arg(long)]
    pub suffix: Option<String>,

    // Paths
    #[arg(long)]
    pub relative: bool,
    #[arg(long)]
    pub no_implied_dirs: bool,

    // Progress
    #[arg(long)]
    pub progress: bool,
    #[arg(short = 'P', long)]
    pub partial_progress: bool,

    // Stats
    #[arg(long)]
    pub stats: bool,

    // Dry run
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    // Size limits
    #[arg(long)]
    pub max_size: Option<String>,
    #[arg(long)]
    pub min_size: Option<String>,

    // Protocol
    #[arg(long)]
    pub protocol: Option<u32>,
    #[arg(long)]
    pub checksum_choice: Option<String>,

    // Batch mode
    #[arg(long, value_name = "FILE")]
    pub write_batch: Option<String>,
    #[arg(long, value_name = "FILE")]
    pub read_batch: Option<String>,

    // Misc
    #[arg(long)]
    pub timeout: Option<u32>,
    #[arg(long)]
    pub bwlimit: Option<u64>,
    #[arg(long)]
    pub fuzzy: bool,
    #[arg(long)]
    pub prune_empty_dirs: bool,
    #[arg(long)]
    pub one_file_system: bool,
    #[arg(long)]
    pub mkpath: bool,
    #[arg(long)]
    pub ignore_errors: bool,
    #[arg(long)]
    pub ignore_existing: bool,
    #[arg(long)]
    pub ignore_non_existing: bool,
    #[arg(long)]
    pub remove_source_files: bool,
    #[arg(long)]
    pub list_only: bool,

    // Positional: source(s) and destination
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl Options {
    /// Expand -a (archive) into its component flags: -rlptgoD
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

    /// Extract (sources, destination) from self.args.
    /// Returns error if fewer than 2 args.
    pub fn parse_paths(&self) -> Result<(Vec<String>, String)> {
        // --read-batch only needs a destination (no source)
        if self.read_batch.is_some() && self.args.len() == 1 {
            return Ok((vec![], self.args[0].clone()));
        }
        if self.args.len() < 2 {
            bail!(
                "need at least 2 arguments (source and destination), got {}",
                self.args.len()
            );
        }
        let dest = self.args.last().unwrap().clone();
        let sources = self.args[..self.args.len() - 1].to_vec();
        Ok((sources, dest))
    }

    /// Generate the server-side option arguments to pass to remote rsync.
    /// This mirrors server_options() in options.c.
    pub fn server_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        // Build a combined short-flag string where possible
        let mut flags = String::from("-");
        if self.verbose > 0 {
            for _ in 0..self.verbose {
                flags.push('v');
            }
        }
        if self.quiet {
            flags.push('q');
        }
        if self.archive {
            flags.push('a');
        } else {
            if self.recursive {
                flags.push('r');
            }
            if self.links {
                flags.push('l');
            }
            if self.perms {
                flags.push('p');
            }
            if self.times {
                flags.push('t');
            }
            if self.group {
                flags.push('g');
            }
            if self.owner {
                flags.push('o');
            }
            if self.devices {
                flags.push('D');
            }
        }
        if self.checksum {
            flags.push('c');
        }
        if self.update {
            flags.push('u');
        }
        if self.hard_links {
            flags.push('H');
        }
        if self.copy_links {
            flags.push('L');
        }
        if self.keep_dirlinks {
            flags.push('k');
        }
        if self.whole_file {
            flags.push('W');
        }
        if self.dry_run {
            flags.push('n');
        }
        if self.relative {
            flags.push('R');
        }
        if self.backup {
            flags.push('b');
        }
        if self.one_file_system {
            flags.push('x');
        }
        if self.itemize_changes {
            flags.push('i');
        }
        if self.omit_dir_times {
            flags.push('O');
        }
        if self.prune_empty_dirs {
            flags.push('m');
        }
        if self.fuzzy {
            flags.push('y');
        }
        if self.compress {
            flags.push('z');
        }

        // Only push if we added at least one flag beyond the leading '-'
        if flags.len() > 1 {
            args.push(flags);
        }

        // Long flags that have no short form
        if self.delete {
            args.push("--delete".into());
        }
        if self.delete_before {
            args.push("--delete-before".into());
        }
        if self.delete_during {
            args.push("--delete-during".into());
        }
        if self.delete_after {
            args.push("--delete-after".into());
        }
        if self.delete_excluded {
            args.push("--delete-excluded".into());
        }
        if self.force {
            args.push("--force".into());
        }
        if let Some(md) = self.max_delete {
            args.push(format!("--max-delete={md}"));
        }
        if self.numeric_ids {
            args.push("--numeric-ids".into());
        }
        if self.copy_dirlinks {
            args.push("--copy-dirlinks".into());
        }
        if self.no_implied_dirs {
            args.push("--no-implied-dirs".into());
        }
        if self.partial {
            args.push("--partial".into());
        }
        if let Some(ref pd) = self.partial_dir {
            args.push(format!("--partial-dir={pd}"));
        }
        if self.inplace {
            args.push("--inplace".into());
        }
        if self.append {
            args.push("--append".into());
        }
        if self.no_whole_file {
            args.push("--no-whole-file".into());
        }
        if self.progress {
            args.push("--progress".into());
        }
        if self.stats {
            args.push("--stats".into());
        }
        if let Some(ref ms) = self.max_size {
            args.push(format!("--max-size={ms}"));
        }
        if let Some(ref ms) = self.min_size {
            args.push(format!("--min-size={ms}"));
        }
        if let Some(bw) = self.bwlimit {
            args.push(format!("--bwlimit={bw}"));
        }
        if let Some(to) = self.timeout {
            args.push(format!("--timeout={to}"));
        }
        if let Some(ref cc) = self.checksum_choice {
            args.push(format!("--checksum-choice={cc}"));
        }
        if let Some(proto) = self.protocol {
            args.push(format!("--protocol={proto}"));
        }
        if let Some(cl) = self.compress_level {
            args.push(format!("--compress-level={cl}"));
        }
        if self.omit_link_times {
            args.push("--omit-link-times".into());
        }
        if self.executability {
            args.push("--executability".into());
        }
        if self.acls {
            args.push("--acls".into());
        }
        if self.xattrs {
            args.push("--xattrs".into());
        }
        if self.specials && !self.archive && !self.devices {
            args.push("--specials".into());
        }
        if self.ignore_errors {
            args.push("--ignore-errors".into());
        }
        if self.ignore_existing {
            args.push("--ignore-existing".into());
        }
        if self.ignore_non_existing {
            args.push("--ignore-non-existing".into());
        }
        if self.remove_source_files {
            args.push("--remove-source-files".into());
        }
        if self.list_only {
            args.push("--list-only".into());
        }
        if self.mkpath {
            args.push("--mkpath".into());
        }
        if let Some(ref sfx) = self.suffix {
            args.push(format!("--suffix={sfx}"));
        }
        if let Some(ref bd) = self.backup_dir {
            args.push(format!("--backup-dir={bd}"));
        }
        if self.cvs_exclude {
            args.push("--cvs-exclude".into());
        }

        // Filter/exclude/include rules are sent as separate arguments
        for pat in &self.exclude {
            args.push(format!("--exclude={pat}"));
        }
        for pat in &self.include_patterns {
            args.push(format!("--include={pat}"));
        }
        for rule in &self.filter {
            args.push("--filter".into());
            args.push(rule.clone());
        }

        args
    }

    /// True if this is a local transfer (no remote host in any source or dest).
    pub fn is_local(&self) -> bool {
        if let Ok((sources, dest)) = self.parse_paths() {
            for src in &sources {
                if Self::parse_remote_src(src).is_some() {
                    return false;
                }
            }
            if Self::parse_remote_dst(&dest).is_some() {
                return false;
            }
        }
        true
    }

    /// Parse source as "[user@]host:path" or "rsync://[user@]host[:port]/path".
    /// Returns None if local path.
    pub fn parse_remote_src(src: &str) -> Option<RemoteSpec> {
        parse_remote(src)
    }

    /// Parse destination similarly.
    pub fn parse_remote_dst(dst: &str) -> Option<RemoteSpec> {
        parse_remote(dst)
    }
}

#[derive(Debug, Clone)]
pub struct RemoteSpec {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
    pub is_daemon: bool,
}

/// Parse "[user@]host:path" or "rsync://[user@]host[:port]/path".
/// Returns None for plain local paths.
fn parse_remote(s: &str) -> Option<RemoteSpec> {
    // rsync:// URL form
    if let Some(rest) = s.strip_prefix("rsync://") {
        return parse_rsync_url(rest);
    }

    // SCP-style: [user@]host:path
    // Must not be an absolute Windows path (C:\...) or start with /
    // Avoid treating a single-char drive letter as a host on Windows
    if s.starts_with('/') || s.starts_with('.') {
        return None;
    }

    // Find ':' that separates host from path
    if let Some(colon) = s.find(':') {
        // If colon is the first char, it's not a remote spec
        if colon == 0 {
            return None;
        }
        // Windows drive letter: single alpha char before ':'
        let before = &s[..colon];
        if before.len() == 1 && before.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
            return None;
        }

        let host_part = &s[..colon];
        let path = s[colon + 1..].to_string();

        let (user, host) = split_user_host(host_part);
        return Some(RemoteSpec {
            user,
            host,
            port: None,
            path,
            is_daemon: false,
        });
    }

    None
}

/// Parse the authority+path part of an rsync:// URL: [user@]host[:port]/path
fn parse_rsync_url(rest: &str) -> Option<RemoteSpec> {
    // Split on first '/'
    let (authority, path) = if let Some(slash) = rest.find('/') {
        (&rest[..slash], rest[slash + 1..].to_string())
    } else {
        (rest, String::new())
    };

    let (user_host, port) = if let Some(bracket_end) = authority.find(']') {
        // IPv6 [::1]:port
        let host_raw = &authority[..=bracket_end];
        let port = if authority.len() > bracket_end + 1 && authority.as_bytes()[bracket_end + 1] == b':' {
            authority[bracket_end + 2..].parse::<u16>().ok()
        } else {
            None
        };
        (host_raw, port)
    } else if let Some(colon) = authority.rfind(':') {
        let port = authority[colon + 1..].parse::<u16>().ok();
        (&authority[..colon], port)
    } else {
        (authority, None)
    };

    if user_host.is_empty() {
        return None;
    }

    let (user, host) = split_user_host(user_host);

    Some(RemoteSpec {
        user,
        host,
        port,
        path,
        is_daemon: true,
    })
}

/// Split "user@host" into (Some("user"), "host"), or (None, "host").
fn split_user_host(s: &str) -> (Option<String>, String) {
    if let Some(at) = s.find('@') {
        let user = s[..at].to_string();
        let host = s[at + 1..].to_string();
        (Some(user), host)
    } else {
        (None, s.to_string())
    }
}
