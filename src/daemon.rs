//! rsync daemon mode (anonymous, single-fork-per-connection).
//!
//! Implements the `@RSYNCD:` greeting protocol, a minimal `rsyncd.conf`
//! parser and module dispatch that hands the socket off to
//! [`crate::run_server_io`].
//!
//! Reference: `clientserver.c::start_daemon`, `loadparm.c`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::options::Options;
use crate::protocol::constants::PROTOCOL_VERSION;

/// One module section in `rsyncd.conf`.
#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub path: PathBuf,
    pub comment: String,
    pub read_only: bool,
    pub list: bool,
}

/// Parsed `rsyncd.conf`.
#[derive(Debug, Clone, Default)]
pub struct DaemonConfig {
    pub motd_file: Option<PathBuf>,
    pub port: Option<u16>,
    pub address: Option<String>,
    pub modules: Vec<Module>,
}

impl DaemonConfig {
    pub fn find(&self, name: &str) -> Option<&Module> {
        self.modules.iter().find(|m| m.name == name)
    }
}

/// Parse `rsyncd.conf` text (very small subset: globals + named sections
/// with `path`, `comment`, `read only`, `list`).
pub fn parse_config_str(s: &str) -> Result<DaemonConfig> {
    let mut cfg = DaemonConfig::default();
    let mut current: Option<(String, BTreeMap<String, String>)> = None;

    fn finish(cur: Option<(String, BTreeMap<String, String>)>, cfg: &mut DaemonConfig) -> Result<()> {
        if let Some((name, kv)) = cur {
            let path = kv
                .get("path")
                .map(PathBuf::from)
                .with_context(|| format!("module '{name}' missing 'path'"))?;
            let comment = kv.get("comment").cloned().unwrap_or_default();
            let read_only = parse_bool(kv.get("read only").map(|s| s.as_str()).unwrap_or("yes"));
            let list = parse_bool(kv.get("list").map(|s| s.as_str()).unwrap_or("yes"));
            cfg.modules.push(Module { name, path, comment, read_only, list });
        }
        Ok(())
    }

    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            finish(current.take(), &mut cfg)?;
            current = Some((rest.trim().to_string(), BTreeMap::new()));
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().to_string();
        if let Some((_, kv)) = current.as_mut() {
            kv.insert(key, val);
        } else {
            // Globals.
            match key.as_str() {
                "motd file" => cfg.motd_file = Some(PathBuf::from(val)),
                "port" => cfg.port = val.parse().ok(),
                "address" => cfg.address = Some(val),
                _ => {}
            }
        }
    }
    finish(current, &mut cfg)?;
    Ok(cfg)
}

pub fn parse_config_file(p: &Path) -> Result<DaemonConfig> {
    let s = std::fs::read_to_string(p)
        .with_context(|| format!("reading daemon config {}", p.display()))?;
    parse_config_str(&s)
}

fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(),
        "yes" | "true" | "1" | "on")
}

/// Daemon entry point — listens on TCP and dispatches connections.
pub fn run_daemon(opts: &Options) -> Result<()> {
    let cfg = if let Some(p) = opts.config.as_deref() {
        parse_config_file(Path::new(p))?
    } else if Path::new("/etc/rsyncd.conf").exists() {
        parse_config_file(Path::new("/etc/rsyncd.conf"))
            .unwrap_or_default()
    } else {
        DaemonConfig::default()
    };

    let port = opts.port.or(cfg.port).unwrap_or(873);
    let bind_addr = cfg.address.clone().unwrap_or_else(|| "0.0.0.0".to_string());

    let listener = TcpListener::bind((bind_addr.as_str(), port))
        .with_context(|| format!("binding to {bind_addr}:{port}"))?;
    eprintln!("rsync-rs daemon listening on {bind_addr}:{port}");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };
        let cfg = cfg.clone();
        // One process per connection so dup2 of fds 0/1 is safe.  On Unix we
        // fork; otherwise we fall back to a thread (Windows daemon support is
        // best-effort and currently untested).
        #[cfg(unix)]
        {
            match unsafe { libc::fork() } {
                -1 => eprintln!("fork failed"),
                0 => {
                    drop(listener_close_in_child());
                    let r = handle_connection(stream, &cfg);
                    if let Err(e) = r {
                        eprintln!("daemon connection error: {e:#}");
                        std::process::exit(1);
                    }
                    std::process::exit(0);
                }
                _pid => {
                    // Parent: close the client socket and continue accept loop.
                    // Reap any finished children non-blockingly.
                    unsafe {
                        let mut status: libc::c_int = 0;
                        while libc::waitpid(-1, &mut status, libc::WNOHANG) > 0 {}
                    }
                    drop(stream);
                }
            }
        }
        #[cfg(not(unix))]
        {
            std::thread::spawn(move || {
                if let Err(e) = handle_connection(stream, &cfg) {
                    eprintln!("daemon connection error: {e:#}");
                }
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn listener_close_in_child() -> () {
    // Placeholder: the listener is dropped when run_daemon returns from fork.
    // We can't drop the listener here (it's owned by caller), but the kernel
    // doesn't pass it to exec'd children.  Closing in-process is harmless.
}

/// Handle a single accepted connection: greet, dispatch, hand off.
fn handle_connection(
    stream: std::net::TcpStream,
    cfg: &DaemonConfig,
) -> Result<()> {
    stream.set_nodelay(true).ok();
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream.try_clone()?);

    // 1. Send daemon greeting.  We don't yet support auth-checksum negotiation,
    // so we omit the trailing checksum list (peers tolerate this).
    write!(writer, "@RSYNCD: {}.0\n", PROTOCOL_VERSION)?;
    writer.flush()?;

    // 2. Read peer greeting.
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    if !line.starts_with("@RSYNCD:") {
        write!(writer, "@ERROR: protocol startup error\n")?;
        return Ok(());
    }

    // 3. Read the module-name line.
    let mut modname = String::new();
    if reader.read_line(&mut modname)? == 0 {
        return Ok(());
    }
    let modname = modname.trim_end_matches(['\n', '\r']).to_string();

    if modname.is_empty() || modname == "#list" {
        if let Some(motd) = &cfg.motd_file {
            if let Ok(text) = std::fs::read_to_string(motd) {
                writer.write_all(text.as_bytes())?;
                if !text.ends_with('\n') {
                    writer.write_all(b"\n")?;
                }
            }
        }
        for m in &cfg.modules {
            if m.list {
                writeln!(writer, "{:<15}\t{}", m.name, m.comment)?;
            }
        }
        write!(writer, "@RSYNCD: EXIT\n")?;
        writer.flush()?;
        return Ok(());
    }

    if modname.starts_with('#') {
        write!(writer, "@ERROR: Unknown command '{modname}'\n")?;
        return Ok(());
    }

    let module = match cfg.find(&modname) {
        Some(m) => m.clone(),
        None => {
            write!(writer, "@ERROR: Unknown module '{modname}'\n")?;
            return Ok(());
        }
    };

    // 4. Approve module access.
    write!(writer, "@RSYNCD: OK\n")?;
    writer.flush()?;

    // BufReader may have over-read; we need raw socket access from now on.
    drop(reader);
    let mut raw_reader = stream.try_clone()?;

    // 5. Read the per-connection argv.  Protocol ≥ 30 uses NUL-terminated
    //    args (ending with an empty string == double-NUL).
    //    C `glob_expand_module()` (util1.c:786) strips the "<module>" or
    //    "<module>/" prefix from path args after the dot-separator, so do the
    //    same here -- otherwise paths like "upload/" resolve under the
    //    already-chdir'd module dir and the receiver writes nowhere.
    let mut args: Vec<String> = vec!["rsyncd".to_string()];
    {
        use std::io::Read;
        let mut byte = [0u8; 1];
        let mut cur: Vec<u8> = Vec::new();
        let mut seen_dot = false;
        loop {
            let n = raw_reader.read(&mut byte)?;
            if n == 0 {
                break;
            }
            if byte[0] == 0 {
                if cur.is_empty() {
                    break;
                }
                let mut s = String::from_utf8_lossy(&cur).into_owned();
                if seen_dot {
                    // C clients sometimes embed the module name itself in the
                    // path arg (e.g. push to bare module: "upload/" or
                    // "upload").  After we chdir into module.path, that
                    // prefix would resolve to a non-existent subdir.  Strip
                    // ONLY when the entire arg is the module name (with or
                    // without a trailing slash) -- mirrors the empty-dir
                    // case of C's util1.c::glob_expand_module.  Do NOT
                    // strip from inside the path (e.g. "upload/foo.txt"):
                    // C clients already strip the leading "MOD/" themselves
                    // for sub-paths in pull and push, so a stripped-here
                    // sub-path would become incorrect.
                    if s == module.name || s == format!("{}/", module.name) {
                        s = ".".to_string();
                    }
                }
                if s == "." {
                    seen_dot = true;
                }
                args.push(s);
                cur.clear();
            } else {
                cur.push(byte[0]);
            }
        }
    }

    // 6. chdir into the module path so relative source paths resolve there.
    std::env::set_current_dir(&module.path)
        .with_context(|| format!("chdir to module path {}", module.path.display()))?;

    // 7. Build server Options and hand off the socket to run_server_io.
    let mut opts2 = Options {
        server: true,
        daemon: true,
        sender: args.iter().any(|a| a == "--sender"),
        args: args.clone(),
        ..Options::default()
    };
    if !module.read_only {
        // Receiver-side daemon writes, read-only=no allows it.
    } else if !opts2.sender {
        write!(writer, "@ERROR: module '{}' is read only\n", module.name)?;
        return Ok(());
    }
    opts2.expand_archive();

    let _ = crate::run_server_io(&opts2, raw_reader, stream)?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let s = r#"
# global comment
motd file = /etc/motd

[data]
    path = /srv/data
    comment = Public data
    read only = yes

[private]
    path = /srv/private
    list = no
"#;
        let cfg = parse_config_str(s).unwrap();
        assert_eq!(cfg.modules.len(), 2);
        assert_eq!(cfg.motd_file, Some(PathBuf::from("/etc/motd")));
        let data = cfg.find("data").unwrap();
        assert_eq!(data.path, PathBuf::from("/srv/data"));
        assert_eq!(data.comment, "Public data");
        assert!(data.read_only);
        assert!(data.list);
        let priv_ = cfg.find("private").unwrap();
        assert!(!priv_.list);
    }

    #[test]
    fn parse_missing_path_errors() {
        let s = "[bad]\ncomment = nopath\n";
        assert!(parse_config_str(s).is_err());
    }

    #[test]
    fn parse_bool_variants() {
        assert!(parse_bool("yes"));
        assert!(parse_bool("YES"));
        assert!(parse_bool("true"));
        assert!(parse_bool("1"));
        assert!(parse_bool("on"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool(""));
    }

    #[test]
    fn parse_globals() {
        let s = "port = 1234\naddress = 127.0.0.1\n[m]\npath = /tmp\n";
        let cfg = parse_config_str(s).unwrap();
        assert_eq!(cfg.port, Some(1234));
        assert_eq!(cfg.address.as_deref(), Some("127.0.0.1"));
    }
}

// silence unused import on non-Unix
#[cfg(not(unix))]
fn _suppress_imports() {}
