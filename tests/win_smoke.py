"""Windows-native smoke test for rsync-rs.

Runs the real rsync-rs.exe (built by `cargo build --release`) and exercises:
  * `--version` & `--help` output
  * `--local` mode: copy a small tree with -a flags
  * `--list-only` mode

This script is invoked from CI on `windows-latest` after `cargo test`.
It is intentionally pure-stdlib so no extra installs are needed.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def find_binary() -> str:
    on_windows = os.name == "nt"
    candidates = (
        [Path("target/release/rsync-rs.exe"), Path("target/release/rsync-rs"),
         Path("target/debug/rsync-rs.exe")]
        if on_windows else
        [Path("target/release/rsync-rs"), Path("target/debug/rsync-rs")]
    )
    for c in candidates:
        if c.exists():
            return str(c.resolve())
    raise SystemExit(f"rsync-rs binary not found in any of: {candidates}")


SEP = os.sep  # use the platform-native trailing separator for source dirs


def run(cmd, **kw) -> subprocess.CompletedProcess:
    print(f"  $ {' '.join(map(str, cmd))}")
    return subprocess.run(cmd, capture_output=True, text=True, timeout=30, **kw)


def hash_tree(root: Path) -> dict[str, str]:
    """Walk root and return a {relpath: sha256-of-content-or-'<dir>'} map."""
    out: dict[str, str] = {}
    for p in sorted(root.rglob("*")):
        rel = p.relative_to(root).as_posix()
        if p.is_dir():
            out[rel] = "<dir>"
        elif p.is_file():
            out[rel] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


def check_version(rs: str) -> None:
    print("[check] --version")
    cp = run([rs, "--version"])
    assert cp.returncode == 0, f"--version exit={cp.returncode} stderr={cp.stderr}"
    assert "rsync" in cp.stdout.lower(), f"unexpected --version stdout: {cp.stdout!r}"
    print(f"  OK: {cp.stdout.splitlines()[0]}")


def check_help(rs: str) -> None:
    print("[check] --help")
    cp = run([rs, "--help"])
    assert cp.returncode == 0, f"--help exit={cp.returncode}"
    assert len(cp.stdout) > 200, f"--help output too short ({len(cp.stdout)} bytes)"
    print(f"  OK: {len(cp.stdout)} bytes of help text")


def check_local_copy(rs: str) -> None:
    print("[check] --local copy of a small tree")
    work = Path(tempfile.mkdtemp(prefix="rsync-rs-winsmoke-"))
    try:
        src = work / "src"
        dst = work / "dst"
        src.mkdir()
        # populate src
        (src / "hello.txt").write_text("hello, world\r\n", encoding="utf-8")
        (src / "data").mkdir()
        (src / "data" / "binary.bin").write_bytes(bytes(range(256)) * 16)  # 4 KiB
        (src / "data" / "empty.txt").write_text("")
        (src / "nested" / "deep").mkdir(parents=True)
        (src / "nested" / "deep" / "file.txt").write_text("deep\n")

        cp = run([rs, "-r", "--", f"{src}{SEP}", f"{dst}{SEP}"])
        if cp.returncode != 0:
            print(f"STDOUT:\n{cp.stdout}\nSTDERR:\n{cp.stderr}")
            raise SystemExit(f"local copy failed with exit {cp.returncode}")

        src_hashes = hash_tree(src)
        dst_hashes = hash_tree(dst)
        if src_hashes != dst_hashes:
            print(f"src tree:\n{src_hashes}\ndst tree:\n{dst_hashes}")
            raise SystemExit("destination tree differs from source")

        print(f"  OK: {len(src_hashes)} entries copied identically")
    finally:
        shutil.rmtree(work, ignore_errors=True)


def check_local_idempotent(rs: str) -> None:
    """Second sync over an already-synced tree must be a no-op (no errors)."""
    print("[check] --local idempotent re-sync")
    work = Path(tempfile.mkdtemp(prefix="rsync-rs-winsmoke-"))
    try:
        src = work / "src"
        dst = work / "dst"
        src.mkdir()
        (src / "a.txt").write_text("a")
        (src / "b.txt").write_text("b" * 1000)
        cp1 = run([rs, "-r", "--", f"{src}{SEP}", f"{dst}{SEP}"])
        assert cp1.returncode == 0, cp1.stderr
        cp2 = run([rs, "-r", "--", f"{src}{SEP}", f"{dst}{SEP}"])
        assert cp2.returncode == 0, f"second sync failed: {cp2.stderr}"
        if hash_tree(src) != hash_tree(dst):
            raise SystemExit("post-resync trees differ")
        print("  OK: re-sync is a no-op")
    finally:
        shutil.rmtree(work, ignore_errors=True)


def check_list_only(rs: str) -> None:
    print("[check] --list-only")
    work = Path(tempfile.mkdtemp(prefix="rsync-rs-winsmoke-"))
    try:
        src = work / "src"
        src.mkdir()
        (src / "x.txt").write_text("x")
        (src / "y.txt").write_text("yy")
        cp = run([rs, "--list-only", "-r", f"{src}{SEP}"])
        if cp.returncode != 0:
            print(f"STDOUT:\n{cp.stdout}\nSTDERR:\n{cp.stderr}")
            raise SystemExit(f"--list-only failed exit={cp.returncode}")
        out = cp.stdout
        assert "x.txt" in out and "y.txt" in out, \
            f"expected file names in listing, got:\n{out}"
        print(f"  OK: listing produced {len(out.splitlines())} lines")
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main() -> int:
    rs = os.environ.get("RSYNC_RS") or find_binary()
    print(f"using rsync-rs at: {rs}\n")
    check_version(rs)
    check_help(rs)
    check_local_copy(rs)
    check_local_idempotent(rs)
    check_list_only(rs)
    print("\nAll Windows smoke checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
