//! Daemon-client transport: connects to a remote `rsync --daemon` over TCP
//! and walks the textual `@RSYNCD:` handshake before handing the socket back
//! to the normal protocol pipeline.
//!
//! Mirrors `clientserver.c::start_inband_exchange` from the upstream C
//! source.  Flow:
//!
//! 1. TCP connect to `host:port` (default 873).
//! 2. Send `@RSYNCD: <ver>.0\n`; receive `@RSYNCD: <peer_ver>[.<sub>]\n`.
//! 3. Send the module name on its own line.
//! 4. Read response lines: each `@RSYNCD: ...` line is either `OK`,
//!    `EXIT`, `AUTHREQD <chal>`, or another informational line; an
//!    `@ERROR ...` line aborts.  Non-`@`-prefixed lines are MOTD / module
//!    listings and are forwarded to stdout.
//! 5. Send NUL-terminated server-side argv ending with an empty arg.
//! 6. Hand control back: the caller resumes with `setup_compat_client`
//!    (we *skip* the binary protocol-version handshake, since the version
//!    came from the textual greeting).
//!
//! We deliberately do **not** implement AUTHREQD — if the remote demands
//! authentication we surface a clean error and let the user move to SSH
//! transport.  Anonymous modules are sufficient for the bulk of public
//! mirrors and for our CI matrix.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::protocol::constants::PROTOCOL_VERSION;

/// Negotiated outcome of the textual greeting.  The TCP stream is split into
/// independent read/write halves (via `try_clone`) so we can keep using the
/// same blocking-IO pattern as the SSH transport.
pub struct DaemonClient {
    pub reader: TcpStream,
    pub writer: TcpStream,
    pub protocol: u32,
}

impl DaemonClient {
    /// Open a connection to `host:port` and complete the daemon greeting +
    /// module + argv exchange.  After this returns, the next bytes on the
    /// wire are the server's compat_flags varint (i.e. exactly where a
    /// post-`protocol_handshake` SSH client would resume).
    ///
    /// `module` is the rsync module name (the path component immediately
    /// after the host in `rsync://host/MOD/...`).
    ///
    /// `server_argv` is the list of args to send to the daemon **excluding**
    /// the leading `--server` literal — we add that ourselves.  The caller
    /// must include `--sender` if pulling, plus the merged short-flag string
    /// (`-aDtpr...`) and the trailing path args.
    pub fn connect(
        host: &str,
        port: u16,
        module: &str,
        server_argv: &[String],
    ) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect(&addr)
            .with_context(|| format!("failed to connect to rsync daemon at {addr}"))?;
        // Reasonable default; daemon may not respond instantly under load.
        // The pipeline itself uses blocking I/O so we leave the timeouts
        // off after the handshake completes.
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_nodelay(true).ok();

        let writer_raw = stream.try_clone().context("clone tcp stream")?;
        let reader_raw = stream.try_clone().context("clone tcp stream")?;
        let mut writer = writer_raw;
        let mut reader = BufReader::new(reader_raw);

        // 1. Send our greeting.
        write!(writer, "@RSYNCD: {}.0\n", PROTOCOL_VERSION)?;
        writer.flush()?;

        // 2. Read peer greeting.  Format: "@RSYNCD: <maj>[.<min>]" or "@ERROR ...".
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("read daemon greeting")?;
        let peer_proto = {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            parse_greeting(trimmed)
                .ok_or_else(|| anyhow!("unexpected daemon greeting: {trimmed:?}"))?
        };
        // Negotiate minimum, clamped to our supported range.
        let protocol = peer_proto.min(PROTOCOL_VERSION);
        if protocol < crate::protocol::constants::MIN_PROTOCOL_VERSION {
            bail!(
                "remote rsync daemon protocol {peer_proto} is too old (need >= {})",
                crate::protocol::constants::MIN_PROTOCOL_VERSION
            );
        }

        // 3. Send module-name line.
        write!(writer, "{module}\n")?;
        writer.flush()?;

        // 4. Read response lines until OK/AUTHREQD/ERROR/EXIT.
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .context("read daemon response")?;
            if n == 0 {
                bail!("rsync daemon closed connection before module approval");
            }
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if let Some(rest) = trimmed.strip_prefix("@RSYNCD: ") {
                if rest == "OK" {
                    break;
                }
                if rest == "EXIT" {
                    bail!("rsync daemon closed listing channel without granting access");
                }
                if let Some(_chal) = rest.strip_prefix("AUTHREQD ") {
                    bail!(
                        "rsync daemon requires authentication for module '{module}', \
                         but rsync-rs does not yet implement daemon auth. \
                         Use SSH transport (host:path) instead."
                    );
                }
                // Other @RSYNCD: lines (e.g. version echo on quirky daemons)
                // — just ignore and keep reading.
                continue;
            }
            if let Some(err) = trimmed.strip_prefix("@ERROR") {
                bail!(
                    "rsync daemon rejected module '{module}':{}",
                    err.strip_prefix(':').unwrap_or(err)
                );
            }
            // MOTD / informational text — forward to stderr so the user
            // sees it (matches C client behavior).
            eprintln!("{trimmed}");
        }

        // BufReader may have over-read; convert it back into the bare stream
        // for downstream binary I/O.
        let buffered = reader.buffer().to_vec();
        if !buffered.is_empty() {
            // We assume the reader was line-buffered against the greeting;
            // there must be no leftover bytes once "@RSYNCD: OK\n" was the
            // last line, because the daemon waits for our argv before
            // proceeding.  Defensive check:
            return Err(anyhow!(
                "daemon sent unexpected bytes after OK: {} bytes",
                buffered.len()
            ));
        }
        let reader_raw = reader.into_inner();

        // 5. Send NUL-terminated argv.  Always lead with "--server".
        write_nul_arg(&mut writer, "--server")?;
        for a in server_argv {
            write_nul_arg(&mut writer, a)?;
        }
        // Trailing empty arg signals end-of-list.
        writer.write_all(&[0u8])?;
        writer.flush()?;

        // Once handshake is done, restore unbounded blocking I/O on both
        // sides — the pipeline uses long-running reads/writes.
        reader_raw.set_read_timeout(None).ok();
        reader_raw.set_write_timeout(None).ok();
        writer.set_read_timeout(None).ok();
        writer.set_write_timeout(None).ok();

        Ok(DaemonClient {
            reader: reader_raw,
            writer,
            protocol,
        })
    }
}

fn write_nul_arg<W: Write>(w: &mut W, arg: &str) -> std::io::Result<()> {
    w.write_all(arg.as_bytes())?;
    w.write_all(&[0u8])?;
    Ok(())
}

/// Parse `@RSYNCD: <maj>[.<sub>]` and return the major protocol version.
fn parse_greeting(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("@RSYNCD: ")?;
    let major = rest.split(['.', ' ']).next()?;
    major.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_greetings() {
        assert_eq!(parse_greeting("@RSYNCD: 31.0"), Some(31));
        assert_eq!(parse_greeting("@RSYNCD: 30"), Some(30));
        assert_eq!(parse_greeting("@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4"), Some(31));
        assert_eq!(parse_greeting("@RSYNCD"), None);
        assert_eq!(parse_greeting("not a greeting"), None);
    }
}
