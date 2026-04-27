//! Spawn an rsync server process locally (both endpoints on the same host).
//!
//! Mirrors the pipe.c / clientserver.c logic for local double-fork: rsync
//! spawns itself with `--server` so the two ends communicate over a pipe pair.

#![allow(dead_code)]

use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result};

pub struct LocalTransport {
    child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

impl LocalTransport {
    /// Spawn `<rsync_path> --server <server_args...> . <path>` as a child process.
    ///
    /// # Arguments
    /// * `rsync_path`  – path to the rsync binary (e.g. `"rsync"` or `"/usr/bin/rsync"`)
    /// * `server_args` – additional server-mode flags (e.g. `["--sender", "-logDtpr"]`)
    /// * `path`        – the remote path argument
    pub fn connect(rsync_path: &str, server_args: &[String], path: &str) -> Result<Self> {
        let mut cmd = Command::new(rsync_path);
        cmd.arg("--server");
        cmd.args(server_args);
        cmd.arg(".");
        cmd.arg(path);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn local rsync: {rsync_path}"))?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        Ok(Self { child, stdin, stdout })
    }

    /// Wait for the child process to exit and return its exit status.
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        // Drop the pipe ends so the child receives EOF and can finish cleanly.
        drop(self.stdin);
        drop(self.stdout);
        self.child.wait().context("waiting for local rsync child")
    }
}
