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

        let mut cmd = Command::new(ssh_cmd);
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
