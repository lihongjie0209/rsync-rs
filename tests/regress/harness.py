"""
Regression harness for rsync-rs.

Provides a small DSL for declaring "scenarios" — repeatable experiments that
exercise rsync-rs against the C reference implementation in every supported
mode (local, remote-shell, daemon) and in both sync directions.

Design goals
------------

* **Declarative.** Each scenario describes its source layout, the rsync
  command-line flags, and the expected post-condition.  The harness handles
  fixture creation, command execution, comparison, and cleanup.
* **Hermetic.** Every test runs in its own temp directory; no global state.
* **Cross-platform-friendly.** Pure-stdlib Python; uses `pathlib`, `shutil`,
  and `subprocess` only.  Daemon and SSH-based scenarios are auto-skipped
  when their prerequisites are missing (so the suite works on Windows too).
* **Loud on failure, quiet on success.** Failed scenarios capture stdout,
  stderr, and a diff of source vs. destination trees.

The harness is intentionally framework-light (no pytest dependency) so it
can be run inside the minimal docker test image without extra installs.
"""

from __future__ import annotations

import dataclasses
import filecmp  # noqa: F401  (handy in scenario assertions)
import hashlib
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import traceback
from pathlib import Path
from typing import Callable, Iterable, Optional, Sequence


# ───────────────────────── ANSI color helpers ──────────────────────────────

_USE_COLOR = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None


def _c(code: str, text: str) -> str:
    return f"\033[{code}m{text}\033[0m" if _USE_COLOR else text


def green(t: str) -> str:  return _c("32", t)
def red(t: str) -> str:    return _c("31", t)
def yellow(t: str) -> str: return _c("33", t)
def cyan(t: str) -> str:   return _c("36", t)
def dim(t: str) -> str:    return _c("2", t)


# ───────────────────────── Fixture builder ──────────────────────────────────


@dataclasses.dataclass
class FileSpec:
    """Describes a file to materialize inside a fixture tree."""
    path: str                          # relative to the fixture root
    content: bytes = b""
    size: Optional[int] = None         # if set, content is replaced with reproducible random bytes
    mode: int = 0o644
    mtime: Optional[float] = None
    symlink_target: Optional[str] = None
    hardlink_to: Optional[str] = None  # path of an earlier entry to hard-link to


@dataclasses.dataclass
class Fixture:
    """A reproducible directory tree built from a list of FileSpec entries."""
    name: str
    files: list[FileSpec]

    def materialize(self, root: Path) -> None:
        root.mkdir(parents=True, exist_ok=True)
        for f in self.files:
            target = root / f.path
            target.parent.mkdir(parents=True, exist_ok=True)
            if f.symlink_target is not None:
                try:
                    if target.exists() or target.is_symlink():
                        target.unlink()
                    os.symlink(f.symlink_target, target)
                except (OSError, NotImplementedError):
                    target.write_bytes(b"<symlink-placeholder>")
                continue
            if f.hardlink_to is not None:
                src = root / f.hardlink_to
                try:
                    if target.exists():
                        target.unlink()
                    os.link(src, target)
                    continue
                except (OSError, NotImplementedError):
                    pass  # fall through to content copy
            if f.size is not None and not f.content:
                # Reproducible "random" content keyed by (fixture name, path).
                seed = f"{self.name}:{f.path}".encode()
                content = bytearray()
                h = hashlib.sha256(seed).digest()
                while len(content) < f.size:
                    content.extend(h)
                    h = hashlib.sha256(h).digest()
                target.write_bytes(bytes(content[: f.size]))
            else:
                target.write_bytes(f.content)
            try:
                os.chmod(target, f.mode)
            except (OSError, NotImplementedError):
                pass
            if f.mtime is not None:
                try:
                    os.utime(target, (f.mtime, f.mtime))
                except OSError:
                    pass


# ───────────────────────── Tree comparison ──────────────────────────────────


def tree_signature(root: Path) -> dict[str, dict]:
    """Return a stable dict mapping relative paths to metadata for diffing."""
    result: dict[str, dict] = {}
    if not root.exists():
        return result
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        for name in filenames:
            full = Path(dirpath) / name
            rel = str(full.relative_to(root)).replace("\\", "/")
            try:
                st = full.lstat()
            except OSError:
                continue
            entry: dict[str, object] = {
                "size": st.st_size,
                "mode": stat.S_IMODE(st.st_mode),
                "type": "link" if stat.S_ISLNK(st.st_mode) else "file",
            }
            if stat.S_ISLNK(st.st_mode):
                try:
                    entry["target"] = os.readlink(full)
                except OSError:
                    entry["target"] = ""
                entry["sha"] = ""
            else:
                try:
                    entry["sha"] = hashlib.sha256(full.read_bytes()).hexdigest()[:16]
                except OSError as e:
                    entry["sha"] = f"err:{e}"
            result[rel] = entry
    return result


def diff_signatures(a: dict, b: dict, *, ignore_mode: bool = False) -> list[str]:
    """Return human-readable list of differences (empty = identical)."""
    diffs: list[str] = []
    keys = sorted(set(a) | set(b))
    for k in keys:
        if k not in a:
            diffs.append(f"+ {k} (only in dst)")
            continue
        if k not in b:
            diffs.append(f"- {k} (only in src)")
            continue
        for field in ("size", "type", "sha", "target"):
            va, vb = a[k].get(field), b[k].get(field)
            if va != vb and not (va is None and vb is None):
                diffs.append(f"~ {k}: {field} {va!r} -> {vb!r}")
        if not ignore_mode and a[k].get("mode") != b[k].get("mode"):
            diffs.append(f"~ {k}: mode {a[k]['mode']:o} -> {b[k]['mode']:o}")
    return diffs


# ───────────────────────── Scenario definition ─────────────────────────────


SyncCallable = Callable[[Path, Path, "ScenarioContext"], subprocess.CompletedProcess]


@dataclasses.dataclass
class ScenarioContext:
    """Information passed into a sync callable."""
    rsync_c: str    = "rsync"
    rsync_rs: str   = "rsync-rs"
    wrapper: str    = "/usr/local/bin/wrapper"
    extra_env: dict = dataclasses.field(default_factory=dict)
    timeout_s: float = 5.0  # default per-subprocess wall-clock cap
    last_stdout: bytes = b""
    last_stderr: bytes = b""
    last_returncode: int = 0


@dataclasses.dataclass
class Scenario:
    name: str
    fixture: Fixture
    sync: SyncCallable                                  # runs the actual rsync
    flags: list[str] = dataclasses.field(default_factory=list)
    expect_exit: int = 0
    ignore_mode: bool = False                           # ignore unix mode in diff
    ignore_paths: list[str] = dataclasses.field(default_factory=list)
    skip_if: Optional[Callable[[], Optional[str]]] = None
    timeout_s: float = 5.0
    setup_dst: Optional[Callable[[Path], None]] = None  # pre-populate dst (for delta tests)
    verify_dst: Optional[Callable[[Path], Optional[str]]] = None  # extra post-sync check; return error string or None


# ───────────────────────── Sync callables (modes) ──────────────────────────


def _run(cmd: Sequence[str], ctx: ScenarioContext, timeout: float = 5.0) -> subprocess.CompletedProcess:
    """Run a command with a hard wall-clock timeout. On timeout we SIGKILL the
    whole process group (subprocess.run with timeout only sends SIGTERM and may
    hang if the child ignores it or has descendants — common for our SSH-style
    rsync wrappers)."""
    env = {**os.environ, **ctx.extra_env}
    start_new_session = (os.name == "posix")
    proc = subprocess.Popen(
        list(cmd),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=start_new_session,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
        rc = proc.returncode
    except subprocess.TimeoutExpired:
        # Hard kill the entire process group to take down any rsync children.
        try:
            if start_new_session:
                import signal
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            else:
                proc.kill()
        except (ProcessLookupError, OSError):
            pass
        try:
            stdout, stderr = proc.communicate(timeout=1)
        except subprocess.TimeoutExpired:
            stdout, stderr = b"", b""
        rc = -9
        stderr = (stderr or b"") + f"\n[harness] killed after {timeout:.1f}s timeout\n".encode()
    completed = subprocess.CompletedProcess(cmd, rc, stdout or b"", stderr or b"")
    ctx.last_stdout, ctx.last_stderr = completed.stdout, completed.stderr
    ctx.last_returncode = completed.returncode
    return completed


def make_sync_local(flags: Sequence[str]) -> SyncCallable:
    """Local mode: rsync-rs <flags> src/ dst/"""
    def go(src: Path, dst: Path, ctx: ScenarioContext) -> subprocess.CompletedProcess:
        return _run([ctx.rsync_rs, *flags, f"{src}/", f"{dst}/"], ctx, timeout=ctx.timeout_s)
    return go


def make_sync_pull_via_wrapper(flags: Sequence[str]) -> SyncCallable:
    """C client pulls from rsync-rs server through the wrapper."""
    def go(src: Path, dst: Path, ctx: ScenarioContext) -> subprocess.CompletedProcess:
        return _run([ctx.rsync_c, *flags, "-e", ctx.wrapper,
                     f"--rsync-path={ctx.rsync_rs}",
                     f"dummy:{src}/", f"{dst}/"], ctx, timeout=ctx.timeout_s)
    return go


def make_sync_push_via_wrapper(flags: Sequence[str]) -> SyncCallable:
    """C client pushes to rsync-rs server."""
    def go(src: Path, dst: Path, ctx: ScenarioContext) -> subprocess.CompletedProcess:
        return _run([ctx.rsync_c, *flags, "-e", ctx.wrapper,
                     f"--rsync-path={ctx.rsync_rs}",
                     f"{src}/", f"dummy:{dst}/"], ctx, timeout=ctx.timeout_s)
    return go


def make_sync_rs_pulls_from_c(flags: Sequence[str]) -> SyncCallable:
    """rsync-rs client pulls from a C rsync server."""
    def go(src: Path, dst: Path, ctx: ScenarioContext) -> subprocess.CompletedProcess:
        return _run([ctx.rsync_rs, *flags, "-e", ctx.wrapper,
                     f"--rsync-path={ctx.rsync_c}",
                     f"dummy:{src}/", f"{dst}/"], ctx, timeout=ctx.timeout_s)
    return go


def make_sync_rs_push_to_c(flags: Sequence[str]) -> SyncCallable:
    """rsync-rs client pushes to a C rsync server (the remote-shell wrapper
    exec's C rsync because of --rsync-path)."""
    def go(src: Path, dst: Path, ctx: ScenarioContext) -> subprocess.CompletedProcess:
        return _run([ctx.rsync_rs, *flags, "-e", ctx.wrapper,
                     f"--rsync-path={ctx.rsync_c}",
                     f"{src}/", f"dummy:{dst}/"], ctx, timeout=ctx.timeout_s)
    return go


def make_sync_self(flags: Sequence[str]) -> SyncCallable:
    """rsync-rs ↔ rsync-rs (both sides)."""
    def go(src: Path, dst: Path, ctx: ScenarioContext) -> subprocess.CompletedProcess:
        return _run([ctx.rsync_rs, *flags, "-e", ctx.wrapper,
                     f"--rsync-path={ctx.rsync_rs}",
                     f"dummy:{src}/", f"{dst}/"], ctx, timeout=ctx.timeout_s)
    return go


# ───────────────────────── Runner ───────────────────────────────────────────


@dataclasses.dataclass
class Result:
    name: str
    status: str   # "pass" | "fail" | "skip"
    message: str = ""
    elapsed_s: float = 0.0


class Runner:
    def __init__(self, *, ctx: Optional[ScenarioContext] = None,
                 work_dir: Optional[Path] = None, verbose: bool = False,
                 filter_re: Optional[re.Pattern] = None,
                 jobs: int = 0):
        self.ctx = ctx or ScenarioContext()
        self.work_root = Path(work_dir) if work_dir else Path(tempfile.mkdtemp(prefix="rsyncrs-regress-"))
        self.work_root.mkdir(parents=True, exist_ok=True)
        self.verbose = verbose
        self.filter_re = filter_re
        self.results: list[Result] = []
        self._lock = __import__("threading").Lock()
        if jobs <= 0:
            jobs = max(2, (os.cpu_count() or 2))
        self.jobs = jobs

    def run(self, scenarios: Iterable[Scenario]) -> int:
        from concurrent.futures import ThreadPoolExecutor, as_completed
        scenarios = [s for s in scenarios
                     if not self.filter_re or self.filter_re.search(s.name)]
        t0 = time.time()
        with ThreadPoolExecutor(max_workers=self.jobs) as ex:
            futs = {ex.submit(self._run_one, sc): sc for sc in scenarios}
            for fut in as_completed(futs):
                # Result already recorded by _run_one; surface unhandled errors.
                exc = fut.exception()
                if exc is not None:
                    sc = futs[fut]
                    with self._lock:
                        self.results.append(Result(sc.name, "fail", f"runner error: {exc}"))
                        self._log(red(f"FAIL {sc.name}") + dim("  (runner error)"))
        wall = time.time() - t0
        self._summarize(wall)
        return 0 if all(r.status != "fail" for r in self.results) else 1

    def _run_one(self, sc: Scenario) -> None:
        if sc.skip_if is not None:
            reason = sc.skip_if()
            if reason:
                with self._lock:
                    self._log(yellow(f"SKIP {sc.name}: {reason}"))
                    self.results.append(Result(sc.name, "skip", reason))
                return

        scratch = self.work_root / sc.name
        if scratch.exists():
            shutil.rmtree(scratch, ignore_errors=True)
        src = scratch / "src"
        dst = scratch / "dst"
        src.mkdir(parents=True)
        dst.mkdir(parents=True)
        sc.fixture.materialize(src)
        if sc.setup_dst:
            sc.setup_dst(dst)

        # Make a per-scenario context copy so concurrent runs don't trample
        # each other's `last_stdout/last_stderr` fields, and so each scenario
        # gets its own subprocess timeout.
        ctx = dataclasses.replace(self.ctx, timeout_s=sc.timeout_s)

        t0 = time.time()
        try:
            proc = sc.sync(src, dst, ctx)
        except subprocess.TimeoutExpired as e:
            self._fail(sc, f"timeout after {sc.timeout_s}s\n{e}")
            return
        except Exception as e:  # noqa: BLE001
            self._fail(sc, f"sync raised: {e}\n{traceback.format_exc()}")
            return
        elapsed = time.time() - t0

        if proc.returncode != sc.expect_exit:
            self._fail(sc,
                f"exit {proc.returncode} (want {sc.expect_exit})\n"
                f"--- STDOUT ---\n{proc.stdout.decode(errors='replace')[-2000:]}\n"
                f"--- STDERR ---\n{proc.stderr.decode(errors='replace')[-2000:]}\n",
                elapsed=elapsed)
            return

        sig_src = tree_signature(src)
        sig_dst = tree_signature(dst)
        for p in sc.ignore_paths:
            sig_src.pop(p, None)
            sig_dst.pop(p, None)
        diffs = diff_signatures(sig_src, sig_dst, ignore_mode=sc.ignore_mode)
        if diffs:
            self._fail(sc, "tree mismatch:\n" + "\n".join(diffs[:40]), elapsed=elapsed)
            return

        if sc.verify_dst is not None:
            err = sc.verify_dst(dst)
            if err:
                self._fail(sc, f"verify_dst: {err}", elapsed=elapsed)
                return

        with self._lock:
            self.results.append(Result(sc.name, "pass", elapsed_s=elapsed))
            self._log(green(f"PASS {sc.name}") + dim(f"  ({elapsed:.2f}s)"))

    def _fail(self, sc: Scenario, msg: str, *, elapsed: float = 0.0) -> None:
        with self._lock:
            self.results.append(Result(sc.name, "fail", msg, elapsed))
            self._log(red(f"FAIL {sc.name}") + dim(f"  ({elapsed:.2f}s)"))
            if self.verbose:
                for line in msg.splitlines():
                    self._log(dim("  " + line))

    def _log(self, msg: str) -> None:
        print(msg, flush=True)

    def _summarize(self, wall_s: float = 0.0) -> None:
        passed = sum(r.status == "pass" for r in self.results)
        failed = sum(r.status == "fail" for r in self.results)
        skipped = sum(r.status == "skip" for r in self.results)
        total = len(self.results)
        line = f"\n  {passed} passed, {failed} failed, {skipped} skipped ({total} total) — {wall_s:.1f}s wall"
        if failed:
            self._log(red(line))
            self._log(red("\nFailures:"))
            for r in self.results:
                if r.status == "fail":
                    self._log(red(f"  • {r.name}"))
                    for ml in r.message.splitlines()[:8]:
                        self._log(dim("      " + ml))
        else:
            self._log(green(line))


# ───────────────────────── Common skip predicates ──────────────────────────


def need_binary(name: str) -> Callable[[], Optional[str]]:
    def check() -> Optional[str]:
        return None if shutil.which(name) else f"missing binary: {name}"
    return check


def need_posix() -> Optional[str]:
    return None if os.name == "posix" else "POSIX-only test"


def need_symlink_support() -> Optional[str]:
    if platform.system() != "Windows":
        return None
    try:
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "link"
            os.symlink("target", p)
        return None
    except OSError:
        return "symlink creation not permitted on this Windows session"
