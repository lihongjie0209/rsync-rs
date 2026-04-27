//! SSH transport — mirrors the pipe.c / clientserver.c remote-shell path.
//!
//! Spawns `ssh [ssh_args] [user@]host rsync --server [server_args] . <path>`
//! and wires stdin/stdout of the ssh child process as the protocol channel.

#![allow(dead_code)]

use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result};

pub struct SshTransport {
    child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

impl SshTransport {
    /// Spawn an SSH session that runs rsync in server mode on the remote host.
    ///
    /// # Arguments
    /// * `ssh_cmd`     – path/name of the SSH binary (e.g. `"ssh"`)
    /// * `ssh_args`    – extra SSH flags (`-p`, `-i`, `-o`, …)
    /// * `host`        – remote hostname or IP address
    /// * `user`        – optional username (`user@host` syntax)
    /// * `rsync_path`  – path to rsync on the remote (default `"rsync"`)
    /// * `server_args` – rsync server-mode flags
    /// * `remote_path` – the remote path argument
    pub fn connect(
        ssh_cmd: &str,
        ssh_args: &[String],
        host: &str,
        user: Option<&str>,
        rsync_path: &str,
        server_args: &[String],
        remote_path: &str,
    ) -> Result<Self> {
        let destination = match user {
            Some(u) => format!("{u}@{host}"),
            None => host.to_owned(),
        };

        // C rsync's `-e <command>` is whitespace-split (with simple quote
        // handling) into program + extra args.  E.g. `-e "ssh -p 2222"` runs
        // `ssh` with `-p 2222 host …`.  Mirror that behaviour.
        let parts = shell_split(ssh_cmd);
        let (program, extra_args) = parts.split_first()
            .ok_or_else(|| anyhow::anyhow!("empty rsh command"))?;

        let mut cmd = Command::new(program);
        cmd.args(extra_args);
        cmd.args(ssh_args);
        cmd.arg(&destination);

        // The remote command: rsync --server <server_args> . <path>
        cmd.arg(rsync_path);
        cmd.arg("--server");
        cmd.args(server_args);
        cmd.arg(".");
        cmd.arg(remote_path);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn ssh: {ssh_cmd} {destination}"))?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        Ok(Self { child, stdin, stdout })
    }

    /// Wait for the SSH child process to exit and return its exit status.
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        drop(self.stdin);
        drop(self.stdout);
        self.child.wait().context("waiting for ssh child")
    }

    /// Move the stdin/stdout pipes out of the transport, returning a handle
    /// that can be used to wait for the child later. Useful when callers want
    /// to wrap the pipes in mux streams (which take ownership).
    pub fn split(self) -> (ChildStdin, ChildStdout, SshChild) {
        let SshTransport { child, stdin, stdout } = self;
        (stdin, stdout, SshChild { child })
    }
}

pub struct SshChild {
    child: Child,
}

impl SshChild {
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        self.child.wait().context("waiting for ssh child")
    }
}

/// Split a shell-style command string into argv tokens.
///
/// Supports single quotes ('…' — literal), double quotes ("…" — also literal
/// for our purposes), and backslash escaping outside of single quotes. This is
/// intentionally simpler than POSIX `sh` quoting but matches what C rsync's
/// own `-e` parser accepts in practice (split on unquoted whitespace).
fn shell_split(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_token = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        // Backslash only acts as an escape when followed by one of the
        // metacharacters we recognise (space, quote, backslash). This keeps
        // Windows paths like `C:\Users\foo` intact while still allowing
        // POSIX-style escapes such as `my\ file`.
        if ch == '\\' && !in_single && !in_double {
            if let Some(&next) = chars.peek() {
                if next == ' ' || next == '\t' || next == '"' || next == '\'' || next == '\\' {
                    cur.push(next);
                    chars.next();
                    has_token = true;
                    continue;
                }
            }
            // Not a recognised escape: keep the backslash literal.
            cur.push('\\');
            has_token = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            has_token = true;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            has_token = true;
            continue;
        }
        if ch.is_whitespace() && !in_single && !in_double {
            if has_token {
                out.push(std::mem::take(&mut cur));
                has_token = false;
            }
            continue;
        }
        cur.push(ch);
        has_token = true;
    }
    if has_token {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::shell_split;

    #[test]
    fn single_word() {
        assert_eq!(shell_split("ssh"), vec!["ssh"]);
    }

    #[test]
    fn two_words() {
        assert_eq!(shell_split("ssh -p2222"), vec!["ssh", "-p2222"]);
    }

    #[test]
    fn quoted_args() {
        assert_eq!(
            shell_split(r#"ssh -o "User=root" -i 'my key.pem'"#),
            vec!["ssh", "-o", "User=root", "-i", "my key.pem"],
        );
    }

    #[test]
    fn backslash_escape() {
        assert_eq!(
            shell_split(r"ssh -i my\ key.pem"),
            vec!["ssh", "-i", "my key.pem"],
        );
    }

    #[test]
    fn windows_unquoted_path_preserved() {
        // Backslashes in Windows paths must survive when not followed by a
        // shell metacharacter.
        assert_eq!(
            shell_split(r"C:\Python\python.exe C:\tmp\rsh.py"),
            vec![r"C:\Python\python.exe", r"C:\tmp\rsh.py"],
        );
    }

    #[test]
    fn empty_arg() {
        assert_eq!(shell_split(""), Vec::<String>::new());
    }

    #[test]
    fn windows_path() {
        // Windows-style program path with embedded space (quoted)
        assert_eq!(
            shell_split(r#""C:\Program Files\OpenSSH\ssh.exe" -p 22"#),
            vec![r"C:\Program Files\OpenSSH\ssh.exe", "-p", "22"],
        );
    }
}
