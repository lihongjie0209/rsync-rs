"""Windows-native interop smoke for rsync-rs.

Goal: prove protocol-level cross-version compatibility on Windows by exercising
the C reference rsync (provided by chocolatey/cwrsync) against rsync-rs.exe via
a stdio "rsh" wrapper.

Scenarios:
  1. C rsync pushes a tree to rsync-rs (rsync-rs is the receiver/server).
  2. C rsync pulls a tree from rsync-rs (rsync-rs is the sender/server).

The wrapper used for `-e` is `python -c ...` that just executes the remote
command locally (ignores the bogus "host" argv), so we keep transport simple
and avoid an actual SSH on Windows.

Failure prints stdout/stderr + a tree diff for fast triage.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Sequence


def find_rsync_rs() -> str:
    env = os.environ.get("RSYNC_RS")
    if env and Path(env).exists():
        return env
    for c in (Path("target/release/rsync-rs.exe"),
              Path("target/release/rsync-rs")):
        if c.exists():
            return str(c.resolve())
    raise SystemExit("rsync-rs(.exe) not found; build with `cargo build --release`")


def find_c_rsync() -> str:
    rs = shutil.which("rsync")
    if not rs:
        raise SystemExit("C rsync not on PATH (install via chocolatey)")
    return rs


def hash_tree(root: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    for p in sorted(root.rglob("*")):
        rel = p.relative_to(root).as_posix()
        if p.is_file():
            out[rel] = hashlib.sha256(p.read_bytes()).hexdigest()
        elif p.is_dir():
            out[rel] = "<dir>"
    return out


def make_wrapper(work: Path) -> tuple[Path, str]:
    """A tiny rsh-replacement script.  rsync invokes it as
       wrapper <host> <remote-cmd...>; we replace argv[0] (typically the C
       rsync's view of `--rsync-path=...`, which MSYS may have path-translated)
       with the canonical Windows-native rsync-rs.exe from $RSYNC_RS, then
       exec the rest. argv is appended to a log file for failure triage.

    Returns (path, rsh_command). The `-e` value is "python_exe wrapper.py";
    rsync splits on whitespace per shell-quoting rules.
    """
    w = work / "rsh.py"
    log = work / "rsh.log"
    trace = work / "rs.trace"
    # IMPORTANT: Cygwin (cwrsync) creates pipes with FILE_FLAG_OVERLAPPED.  A
    # native Windows process (rsync-rs.exe) calling ReadFile on such a handle
    # gets ERROR_INVALID_PARAMETER (87).  We bridge bytes via threads between
    # the inherited Cygwin pipes (sys.stdin/sys.stdout) and a fresh pair of
    # native pipes provided by subprocess.PIPE.
    w.write_text(
        "import os, sys, subprocess, threading, datetime\n"
        f"LOG = r'{log}'\n"
        f"RS  = r'{os.environ.get('RSYNC_RS', '')}'\n"
        f"TRACE = r'{trace}'\n"
        "with open(LOG, 'a', encoding='utf-8') as f:\n"
        "    f.write(f'[{datetime.datetime.now().isoformat()}] argv={sys.argv!r}\\n')\n"
        "args = list(sys.argv[2:])\n"
        "if RS:\n"
        "    args[0] = RS\n"
        "with open(LOG, 'a', encoding='utf-8') as f:\n"
        "    f.write(f'  exec={args!r}\\n')\n"
        "env = os.environ.copy()\n"
        "env['RSYNC_RS_TRACE'] = TRACE\n"
        "p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,\n"
        "                     stderr=sys.stderr, env=env, bufsize=0)\n"
        "in_fd  = sys.stdin.buffer.raw if hasattr(sys.stdin, 'buffer') else sys.stdin\n"
        "out_fd = sys.stdout.buffer.raw if hasattr(sys.stdout, 'buffer') else sys.stdout\n"
        "def pump(src, dst, label):\n"
        "    try:\n"
        "        while True:\n"
        "            buf = src.read(65536)\n"
        "            if not buf:\n"
        "                break\n"
        "            dst.write(buf)\n"
        "            try: dst.flush()\n"
        "            except Exception: pass\n"
        "    except Exception as e:\n"
        "        with open(LOG, 'a', encoding='utf-8') as f:\n"
        "            f.write(f'  pump {label} err: {e}\\n')\n"
        "    finally:\n"
        "        try: dst.close()\n"
        "        except Exception: pass\n"
        "t1 = threading.Thread(target=pump, args=(in_fd, p.stdin, 'in->child'), daemon=True)\n"
        "t2 = threading.Thread(target=pump, args=(p.stdout, out_fd, 'child->out'), daemon=True)\n"
        "t1.start(); t2.start()\n"
        "rc = p.wait()\n"
        "t2.join(timeout=5)\n"
        "with open(LOG, 'a', encoding='utf-8') as f:\n"
        "    f.write(f'  child exit rc={rc}\\n')\n"
        "sys.exit(rc)\n",
        encoding="utf-8",
    )
    py = sys.executable or shutil.which("python") or shutil.which("python3")
    if not py:
        raise SystemExit("no python interpreter found for wrapper")
    return w, f'{py} {w}'


def run(cmd: Sequence[str], **kw) -> subprocess.CompletedProcess:
    print(f"  $ {' '.join(map(str, cmd))}")
    return subprocess.run(cmd, capture_output=True, text=True,
                          timeout=60, **kw)


def populate_src(src: Path) -> None:
    src.mkdir(parents=True, exist_ok=True)
    (src / "hello.txt").write_text("hello, interop\n")
    (src / "data").mkdir()
    (src / "data" / "binary.bin").write_bytes(bytes(range(256)) * 16)
    (src / "nested" / "deep").mkdir(parents=True)
    (src / "nested" / "deep" / "deep.txt").write_text("deep\n")


def assert_match(label: str, src: Path, dst: Path,
                 cp: subprocess.CompletedProcess,
                 rsh_log: Path | None = None,
                 rs_trace: Path | None = None) -> None:
    if cp.returncode != 0:
        print(f"FAIL {label}: exit={cp.returncode}")
        print("STDOUT:\n" + cp.stdout)
        print("STDERR:\n" + cp.stderr)
        if rsh_log is not None and rsh_log.exists():
            print("RSH-LOG:\n" + rsh_log.read_text(encoding="utf-8", errors="replace"))
        if rs_trace is not None and rs_trace.exists():
            print("RS-TRACE:\n" + rs_trace.read_text(encoding="utf-8", errors="replace"))
        raise SystemExit(2)
    sh = hash_tree(src)
    dh = hash_tree(dst)
    if sh != dh:
        only_src = set(sh) - set(dh)
        only_dst = set(dh) - set(sh)
        diff = {k: (sh.get(k), dh.get(k)) for k in (set(sh) | set(dh))
                if sh.get(k) != dh.get(k)}
        print(f"FAIL {label}: trees differ. only-src={only_src}, "
              f"only-dst={only_dst}, diff={diff}")
        raise SystemExit(3)
    print(f"  OK {label}: {len(sh)} entries")


def winpath_to_msys(p: str) -> str:
    """`C:\\foo\\bar` -> `/c/foo/bar` for MSYS-rsync (Chocolatey rsync) local args."""
    p = str(p).replace("\\", "/")
    if len(p) >= 2 and p[1] == ":":
        p = "/" + p[0].lower() + p[2:]
    return p


def winpath_native(p: str) -> str:
    """`C:\\foo\\bar` -> `C:/foo/bar` -- forward slashes, but native drive form
    so that the Windows-native rsync-rs receiver can open it. cwrsync passes
    remote-side paths through verbatim, so this string survives intact."""
    return str(p).replace("\\", "/")


def main() -> int:
    rs = find_rsync_rs()
    rc = find_c_rsync()
    print(f"rsync-rs: {rs}")
    print(f"C rsync : {rc}")

    work = Path(tempfile.mkdtemp(prefix="rsync-rs-winterop-"))
    try:
        _wrapper, rsh = make_wrapper(work)
        rsh_log = work / "rsh.log"
        rs_trace = work / "rs.trace"

        # Scenario 1: C rsync pushes -> rsync-rs receives
        print("\n[scenario] C-push -> rsync-rs receive")
        s1_src = work / "s1_src"; s1_dst = work / "s1_dst"
        populate_src(s1_src)
        cp = run([
            rc, "-r", "-e", rsh,
            f"--rsync-path={rs}",
            f"{winpath_to_msys(str(s1_src))}/",
            f"dummyhost:{winpath_native(str(s1_dst))}/",
        ])
        assert_match("C-push", s1_src, s1_dst, cp, rsh_log, rs_trace)

        # Scenario 2: C rsync pulls <- rsync-rs sends
        print("\n[scenario] C-pull <- rsync-rs send")
        s2_src = work / "s2_src"; s2_dst = work / "s2_dst"
        populate_src(s2_src)
        cp = run([
            rc, "-r", "-e", rsh,
            f"--rsync-path={rs}",
            f"dummyhost:{winpath_native(str(s2_src))}/",
            f"{winpath_to_msys(str(s2_dst))}/",
        ])
        assert_match("C-pull", s2_src, s2_dst, cp, rsh_log, rs_trace)

        # Scenario 3: rsync-rs <-> rsync-rs (self loopback)
        print("\n[scenario] rsync-rs self-loopback (push)")
        s3_src = work / "s3_src"; s3_dst = work / "s3_dst"
        populate_src(s3_src)
        cp = run([
            rs, "-r", "-e", rsh,
            f"--rsync-path={rs}",
            f"{winpath_native(str(s3_src))}/",
            f"dummyhost:{winpath_native(str(s3_dst))}/",
        ])
        assert_match("rs-self-push", s3_src, s3_dst, cp, rsh_log, rs_trace)

        print("\nAll Windows interop scenarios passed.")
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
