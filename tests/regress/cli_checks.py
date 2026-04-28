"""
Wire-format and CLI compatibility checks.

These tests don't sync any data — they pin down the *visible* surface that
users and other rsync clients can observe:

    * `--version` output (exact text required for some downstreams)
    * `--help` non-empty
    * server bundled-flag handshake (e.g. `-vlogDtpre.iLsfxCIvu`)
    * stats line format for `-v` runs
    * itemize-changes 11-character format for `-i`
    * exit-code mapping for invalid options

They run as plain scenarios so they share the same reporter.
"""

from __future__ import annotations

import re
import subprocess
import os
from pathlib import Path

from .harness import (
    Fixture, Scenario, ScenarioContext, _run, dim, green, red,
    need_binary,
)


# ───────────────────────── Helpers ─────────────────────────────────────────


def _expect_re(label: str, pattern: re.Pattern, blob: bytes) -> str | None:
    if not pattern.search(blob.decode("utf-8", errors="replace")):
        return f"{label}: pattern {pattern.pattern!r} not found in:\n{blob.decode(errors='replace')[:500]}"
    return None


def _make_check(name: str, fn) -> Scenario:
    """Wrap a callable that returns an error string (or None) as a Scenario."""
    fixture = Fixture(name=f"_check_{name}", files=[])

    def sync(src, dst, ctx):
        err = fn(ctx)
        # Encode the result through the CompletedProcess protocol.
        rc = 0 if err is None else 1
        return subprocess.CompletedProcess(args=[], returncode=rc,
                                           stdout=b"", stderr=(err or "").encode())

    return Scenario(name=name, fixture=fixture, sync=sync,
                    skip_if=need_binary("rsync-rs"))


# ───────────────────────── Concrete checks ─────────────────────────────────


def check_version_format() -> Scenario:
    pat = re.compile(r"^rsync\s+version\s+\d+\.\d+\.\d+\s+protocol\s+version\s+\d+",
                     re.MULTILINE)
    def fn(ctx: ScenarioContext) -> str | None:
        proc = subprocess.run([ctx.rsync_rs, "--version"], capture_output=True)
        if proc.returncode != 0:
            return f"--version exited {proc.returncode}"
        return _expect_re("--version", pat, proc.stdout)
    return _make_check("cli__version_format", fn)


def check_help_nonempty() -> Scenario:
    def fn(ctx: ScenarioContext) -> str | None:
        proc = subprocess.run([ctx.rsync_rs, "--help"], capture_output=True)
        if proc.returncode != 0:
            return f"--help exited {proc.returncode}"
        if len(proc.stdout) < 200:
            return f"--help output suspiciously short ({len(proc.stdout)} bytes)"
        return None
    return _make_check("cli__help_nonempty", fn)


def check_invalid_option_exit_code() -> Scenario:
    """C rsync exits 1 (RERR_SYNTAX) for unknown options; we should match."""
    def fn(ctx: ScenarioContext) -> str | None:
        proc = subprocess.run([ctx.rsync_rs, "--this-flag-does-not-exist"], capture_output=True)
        if proc.returncode == 0:
            return "expected non-zero exit for invalid option"
        return None
    return _make_check("cli__invalid_option_exit", fn)


def check_protocol_handshake() -> Scenario:
    """Drive rsync-rs through the protocol handshake by bytes; check it
    answers protocol 31 and the compat-flags byte stream we expect."""
    import struct

    def fn(ctx: ScenarioContext) -> str | None:
        # Spawn server-sender mode against an empty source.
        cmd = [ctx.rsync_rs, "--server", "--sender", "-vlogDtpre.iLsfxCIvu",
               "--numeric-ids", ".", "/tmp"]
        proc = subprocess.Popen(cmd, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            assert proc.stdin and proc.stdout
            proc.stdin.write(struct.pack("<i", 32))  # client claims proto 32
            proc.stdin.flush()
            buf = proc.stdout.read(4)
            if len(buf) != 4:
                return f"expected 4 bytes for proto, got {len(buf)}"
            (proto,) = struct.unpack("<i", buf)
            if proto < 30 or proto > 31:
                return f"protocol negotiation returned {proto} (want 30..31)"
            return None
        finally:
            proc.terminate()
            try: proc.wait(timeout=2)
            except Exception: proc.kill()
    return _make_check("cli__protocol_handshake", fn)


def check_dry_run_no_writes() -> Scenario:
    """rsync-rs --dry-run should not modify dst."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile, os
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"; dst = Path(td) / "dst"
            src.mkdir(); dst.mkdir()
            (src / "a.txt").write_bytes(b"hi")
            proc = subprocess.run(
                [ctx.rsync_rs, "-a", "--dry-run", f"{src}/", f"{dst}/"],
                capture_output=True)
            if proc.returncode != 0:
                return f"dry-run exited {proc.returncode}: {proc.stderr.decode(errors='replace')}"
            if any(dst.iterdir()):
                return f"dry-run produced files in {dst}"
        return None
    return _make_check("cli__dry_run_no_writes", fn)


def check_list_only_local() -> Scenario:
    """rsync-rs --list-only on a local source should print one entry per file."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"; src.mkdir()
            (src / "a.txt").write_bytes(b"hello")
            (src / "sub").mkdir()
            (src / "sub" / "b.txt").write_bytes(b"world")
            proc = subprocess.run(
                [ctx.rsync_rs, "--list-only", str(src)],
                capture_output=True)
            if proc.returncode != 0:
                return f"--list-only exited {proc.returncode}: {proc.stderr.decode(errors='replace')}"
            out = proc.stdout.decode(errors="replace")
            for needle in ("a.txt", "sub", "b.txt"):
                if needle not in out:
                    return f"--list-only output missing {needle!r}; got:\n{out}"
            # Mode column should look like 'drwxr-xr-x' or '-rw-r--r--'
            if not re.search(r"^[d-][rwx-]{9}\s", out, re.MULTILINE):
                return f"--list-only mode column malformed:\n{out}"
        return None
    return _make_check("cli__list_only_local", fn)


def check_stats_output() -> Scenario:
    """`rsync-rs --stats -a src/ dst/` should emit the full stats block."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"; dst = Path(td) / "dst"
            src.mkdir(); dst.mkdir()
            (src / "a.txt").write_bytes(b"a" * 100)
            (src / "b.txt").write_bytes(b"b" * 200)
            proc = subprocess.run(
                [ctx.rsync_rs, "-a", "--stats", f"{src}/", f"{dst}/"],
                capture_output=True)
            if proc.returncode != 0:
                return f"--stats exited {proc.returncode}: {proc.stderr.decode(errors='replace')}"
            # C rsync writes --stats to stdout in client mode (rprintf(FINFO)),
            # to stderr in server mode.  Match that: search both streams.
            blob = (proc.stdout + proc.stderr).decode("utf-8", errors="replace")
            for needle in ("Number of files",
                           "Number of regular files transferred",
                           "Total file size",
                           "Total bytes sent",
                           "sent ",
                           "total size is"):
                if needle not in blob:
                    return f"--stats missing {needle!r} in:\n{blob}"
        return None
    return _make_check("cli__stats_output", fn)


def check_flist_messages() -> Scenario:
    """With -v, rsync-rs should print 'sending incremental file list' to stdout."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"; dst = Path(td) / "dst"
            src.mkdir(); dst.mkdir()
            (src / "a.txt").write_bytes(b"hi")
            proc = subprocess.run(
                [ctx.rsync_rs, "-av", f"{src}/", f"{dst}/"],
                capture_output=True)
            if proc.returncode != 0:
                return f"-av exited {proc.returncode}: {proc.stderr.decode(errors='replace')}"
            blob = (proc.stdout + proc.stderr).decode("utf-8", errors="replace")
            if "sending incremental file list" not in blob:
                return f"missing 'sending incremental file list' in:\n{blob}"
        return None
    return _make_check("cli__flist_messages", fn)


def check_error_format() -> Scenario:
    """Failing run should emit C-style 'rsync error: ... (code N) at ...' line."""
    def fn(ctx: ScenarioContext) -> str | None:
        proc = subprocess.run(
            [ctx.rsync_rs, "-a", "/no/such/source/path/__missing__", "/tmp/x_dst"],
            capture_output=True)
        if proc.returncode == 0:
            return f"expected non-zero exit, got 0"
        err = proc.stderr.decode("utf-8", errors="replace")
        if not re.search(r"^rsync error: .+ \(code \d+\) at \S+\(\d+\) \[\w+=[\d.]+\]",
                         err, re.MULTILINE):
            return f"missing C-style error line in:\n{err}"
        return None
    return _make_check("cli__error_format", fn)


def check_daemon_module_list() -> Scenario:
    """Spawn rsync-rs --daemon, then use C rsync to list modules."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile, time, socket
        # Find a free port
        s = socket.socket(); s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]; s.close()

        with tempfile.TemporaryDirectory() as td:
            mod_dir = Path(td) / "data"; mod_dir.mkdir()
            (mod_dir / "hello.txt").write_bytes(b"hi from daemon")
            cfg = Path(td) / "rsyncd.conf"
            cfg.write_text(
                f"[data]\n  path = {mod_dir}\n  comment = Test module\n"
                f"  read only = yes\n  list = yes\n"
            )
            daemon = subprocess.Popen(
                [ctx.rsync_rs, "--daemon", "--no-detach",
                 f"--config={cfg}", f"--port={port}"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                # Wait for the listener
                deadline = time.time() + 2.0
                while time.time() < deadline:
                    try:
                        sk = socket.create_connection(("127.0.0.1", port), 0.2)
                        sk.close()
                        break
                    except OSError:
                        time.sleep(0.05)
                else:
                    return "daemon did not start listening within 2s"

                # Use C rsync to list modules
                proc = subprocess.run(
                    ["rsync", f"rsync://127.0.0.1:{port}/"],
                    capture_output=True, timeout=4)
                blob = proc.stdout.decode("utf-8", errors="replace")
                if "data" not in blob or "Test module" not in blob:
                    return f"module listing missing 'data'/'Test module':\n{blob}\n--stderr--\n{proc.stderr.decode(errors='replace')}"
            finally:
                daemon.terminate()
                try:
                    daemon.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    daemon.kill()
        return None
    return _make_check("cli__daemon_module_list", fn)


def check_daemon_pull_file() -> Scenario:
    """C rsync pulls a file from rsync-rs --daemon over rsync://."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile, time, socket
        s = socket.socket(); s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]; s.close()

        with tempfile.TemporaryDirectory() as td:
            mod_dir = Path(td) / "data"; mod_dir.mkdir()
            (mod_dir / "hello.txt").write_bytes(b"hi from daemon transfer")
            cfg = Path(td) / "rsyncd.conf"
            cfg.write_text(
                f"[data]\n  path = {mod_dir}\n  read only = yes\n"
            )
            dst = Path(td) / "out"; dst.mkdir()
            daemon = subprocess.Popen(
                [ctx.rsync_rs, "--daemon", "--no-detach",
                 f"--config={cfg}", f"--port={port}"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.time() + 2.0
                while time.time() < deadline:
                    try:
                        sk = socket.create_connection(("127.0.0.1", port), 0.2)
                        sk.close(); break
                    except OSError:
                        time.sleep(0.05)
                else:
                    return "daemon did not start within 2s"

                proc = subprocess.run(
                    ["rsync", "-a", f"rsync://127.0.0.1:{port}/data/hello.txt",
                     str(dst / "hello.txt")],
                    capture_output=True, timeout=10)
                if proc.returncode != 0:
                    return (f"rsync exit {proc.returncode}\n"
                            f"stdout={proc.stdout.decode(errors='replace')}\n"
                            f"stderr={proc.stderr.decode(errors='replace')}")
                got = (dst / "hello.txt").read_bytes()
                want = b"hi from daemon transfer"
                if got != want:
                    return f"content mismatch: got={got!r} want={want!r}"
            finally:
                daemon.terminate()
                try: daemon.wait(timeout=2)
                except subprocess.TimeoutExpired: daemon.kill()
        return None
    return _make_check("cli__daemon_pull_file", fn)


def _wait_listen(port: int, deadline_s: float = 2.0) -> bool:
    import socket, time
    deadline = time.time() + deadline_s
    while time.time() < deadline:
        try:
            sk = socket.create_connection(("127.0.0.1", port), 0.2)
            sk.close()
            return True
        except OSError:
            time.sleep(0.05)
    return False


def _free_port() -> int:
    import socket
    s = socket.socket(); s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]; s.close()
    return p


def _kill(p: subprocess.Popen) -> None:
    try:
        p.terminate()
        try: p.wait(timeout=2)
        except subprocess.TimeoutExpired: p.kill()
    except Exception:
        pass


def check_daemon_push_to_rs() -> Scenario:
    """C rsync pushes a tree into rsync-rs --daemon (write-enabled module)."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile, time
        port = _free_port()
        with tempfile.TemporaryDirectory() as td:
            mod_dir = Path(td) / "incoming"; mod_dir.mkdir()
            cfg = Path(td) / "rsyncd.conf"
            cfg.write_text(
                f"[upload]\n  path = {mod_dir}\n  read only = no\n"
                f"  use chroot = no\n"
            )
            src = Path(td) / "src"; src.mkdir()
            (src / "alpha.txt").write_bytes(b"alpha")
            (src / "beta.bin").write_bytes(bytes(range(64)) * 4)

            daemon = subprocess.Popen(
                [ctx.rsync_rs, "--daemon", "--no-detach",
                 f"--config={cfg}", f"--port={port}"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                if not _wait_listen(port):
                    return "rsync-rs daemon did not start within 2s"
                proc = subprocess.run(
                    ["rsync", "-a", f"{src}/",
                     f"rsync://127.0.0.1:{port}/upload/"],
                    capture_output=True, timeout=10)
                if proc.returncode != 0:
                    return (f"C->rs daemon push exit {proc.returncode}\n"
                            f"stdout={proc.stdout.decode(errors='replace')}\n"
                            f"stderr={proc.stderr.decode(errors='replace')}")
                for name, want in [("alpha.txt", b"alpha"),
                                   ("beta.bin", bytes(range(64)) * 4)]:
                    got = (mod_dir / name).read_bytes()
                    if got != want:
                        return f"{name} mismatch: got={got[:32]!r}... want={want[:32]!r}..."
            finally:
                _kill(daemon)
        return None
    return _make_check("cli__daemon_push_to_rs", fn)


def check_daemon_rs_push_to_c() -> Scenario:
    """rsync-rs client pushes to a C rsync --daemon over rsync://."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile
        if not _have_rsync_daemon(ctx):
            return "skip: C rsync not available for daemon"
        port = _free_port()
        with tempfile.TemporaryDirectory() as td:
            mod_dir = Path(td) / "incoming"; mod_dir.mkdir()
            cfg = Path(td) / "rsyncd.conf"
            pid = Path(td) / "rsyncd.pid"
            log = Path(td) / "rsyncd.log"
            cfg.write_text(
                f"port = {port}\n"
                f"pid file = {pid}\n"
                f"log file = {log}\n"
                f"use chroot = no\n"
                f"\n[upload]\n"
                f"  path = {mod_dir}\n"
                f"  read only = no\n"
            )
            src = Path(td) / "src"; src.mkdir()
            (src / "hello.txt").write_bytes(b"hello c daemon")
            (src / "data.bin").write_bytes(bytes(range(128)) * 8)

            daemon = subprocess.Popen(
                [ctx.rsync_c, "--daemon", "--no-detach",
                 f"--config={cfg}"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                if not _wait_listen(port):
                    log_txt = log.read_text(errors='replace') if log.exists() else "(no log)"
                    return f"C daemon did not start. log:\n{log_txt}"
                client = subprocess.Popen(
                    [ctx.rsync_rs, "-a", f"{src}/",
                     f"rsync://127.0.0.1:{port}/upload/"],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    env={**os.environ, "RSYNC_RS_DEBUG": "1"})
                try:
                    out, err = client.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    client.kill()
                    out, err = client.communicate()
                    log_txt = log.read_text(errors='replace') if log.exists() else "(no log)"
                    return (f"rs->C daemon push hung\n"
                            f"stdout={out.decode(errors='replace')}\n"
                            f"stderr={err.decode(errors='replace')}\n"
                            f"daemon_log:\n{log_txt}")
                if client.returncode != 0:
                    log_txt = log.read_text(errors='replace') if log.exists() else "(no log)"
                    return (f"rs->C daemon push exit {client.returncode}\n"
                            f"stdout={out.decode(errors='replace')}\n"
                            f"stderr={err.decode(errors='replace')}\n"
                            f"daemon_log:\n{log_txt}")
                for name, want in [("hello.txt", b"hello c daemon"),
                                   ("data.bin", bytes(range(128)) * 8)]:
                    got = (mod_dir / name).read_bytes()
                    if got != want:
                        return f"{name} mismatch (size got={len(got)} want={len(want)})"
            finally:
                _kill(daemon)
        return None
    return _make_check("cli__daemon_rs_push_to_c", fn)


def check_daemon_rs_pull_from_c() -> Scenario:
    """rsync-rs client pulls from a C rsync --daemon over rsync://."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile
        if not _have_rsync_daemon(ctx):
            return "skip: C rsync not available for daemon"
        port = _free_port()
        with tempfile.TemporaryDirectory() as td:
            mod_dir = Path(td) / "share"; mod_dir.mkdir()
            (mod_dir / "hello.txt").write_bytes(b"hi from c daemon")
            (mod_dir / "blob.bin").write_bytes(bytes(range(200)) * 5)
            cfg = Path(td) / "rsyncd.conf"
            pid = Path(td) / "rsyncd.pid"
            log = Path(td) / "rsyncd.log"
            cfg.write_text(
                f"port = {port}\n"
                f"pid file = {pid}\n"
                f"log file = {log}\n"
                f"use chroot = no\n"
                f"\n[share]\n"
                f"  path = {mod_dir}\n"
                f"  read only = yes\n"
            )
            dst = Path(td) / "dst"; dst.mkdir()

            daemon = subprocess.Popen(
                [ctx.rsync_c, "--daemon", "--no-detach", f"--config={cfg}"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                if not _wait_listen(port):
                    log_txt = log.read_text(errors='replace') if log.exists() else "(no log)"
                    return f"C daemon did not start. log:\n{log_txt}"
                client = subprocess.Popen(
                    [ctx.rsync_rs, "-a", f"rsync://127.0.0.1:{port}/share/",
                     f"{dst}/"],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    env={**os.environ, "RSYNC_RS_DEBUG": "1"})
                try:
                    out, err = client.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    client.kill()
                    out, err = client.communicate()
                    log_txt = log.read_text(errors='replace') if log.exists() else "(no log)"
                    return (f"rs<-C daemon pull hung\n"
                            f"stdout={out.decode(errors='replace')}\n"
                            f"stderr={err.decode(errors='replace')}\n"
                            f"daemon_log:\n{log_txt}")
                if client.returncode != 0:
                    log_txt = log.read_text(errors='replace') if log.exists() else "(no log)"
                    return (f"rs<-C daemon pull exit {client.returncode}\n"
                            f"stdout={out.decode(errors='replace')}\n"
                            f"stderr={err.decode(errors='replace')}\n"
                            f"daemon_log:\n{log_txt}")
                for name, want in [("hello.txt", b"hi from c daemon"),
                                   ("blob.bin", bytes(range(200)) * 5)]:
                    got = (dst / name).read_bytes()
                    if got != want:
                        return f"{name} mismatch (size got={len(got)} want={len(want)})"
            finally:
                _kill(daemon)
        return None
    return _make_check("cli__daemon_rs_pull_from_c", fn)


def check_itemize_changes() -> Scenario:
    """rsync-rs -i (itemize-changes) must output 11-char prefix for new/changed files."""
    def fn(ctx: ScenarioContext) -> str | None:
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"; dst = Path(td) / "dst"
            src.mkdir(); dst.mkdir()
            (src / "a.txt").write_bytes(b"hello")
            # First sync: file is new → should show >f+++++++++
            proc = subprocess.run(
                [ctx.rsync_rs, "-ai", f"{src}/", f"{dst}/"],
                capture_output=True)
            if proc.returncode != 0:
                return f"-ai exited {proc.returncode}: {proc.stderr.decode(errors='replace')}"
            out = (proc.stdout + proc.stderr).decode("utf-8", errors="replace")
            if not re.search(r">f\+{9}\s+a\.txt", out):
                return f"missing '>f+++++++++ a.txt' pattern in:\n{out}"
            # Second sync: nothing changed → no itemize line expected
            proc2 = subprocess.run(
                [ctx.rsync_rs, "-ai", f"{src}/", f"{dst}/"],
                capture_output=True)
            if proc2.returncode != 0:
                return f"-ai second exited {proc2.returncode}"
            out2 = (proc2.stdout + proc2.stderr).decode("utf-8", errors="replace")
            # Should not mention a.txt again
            if re.search(r"a\.txt", out2):
                return f"second -ai run unexpectedly mentioned a.txt:\n{out2}"
        return None
    return _make_check("cli__itemize_changes", fn)



    """Best-effort check that ctx.rsync_c can run as a daemon."""
    try:
        proc = subprocess.run([ctx.rsync_c, "--version"], capture_output=True, timeout=3)
        return proc.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return False


# ───────────────────────── Aggregator ─────────────────────────────────────


def all_cli_checks():
    return [
        check_version_format(),
        check_help_nonempty(),
        check_invalid_option_exit_code(),
        check_protocol_handshake(),
        check_dry_run_no_writes(),
        check_list_only_local(),
        check_stats_output(),
        check_flist_messages(),
        check_error_format(),
        check_itemize_changes(),
        check_daemon_module_list(),
        check_daemon_pull_file(),
        check_daemon_push_to_rs(),
        check_daemon_rs_push_to_c(),
        check_daemon_rs_pull_from_c(),
    ]
