"""
Concrete regression scenarios.

Each entry pairs a fixture (the source layout) with a sync mode (the role of
rsync-rs in the conversation) and the expected post-condition.  The catalogue
is split into orthogonal axes so we get cartesian coverage:

    fixtures × modes × flag-sets

The full matrix is large; `all_scenarios()` returns the curated subset that
balances coverage with run-time.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Iterable

from .harness import (
    FileSpec, Fixture, Scenario,
    make_sync_local, make_sync_pull_via_wrapper, make_sync_push_via_wrapper,
    make_sync_rs_pulls_from_c, make_sync_rs_push_to_c, make_sync_self,
    make_sync_batch, make_sync_backup_dir,
    need_binary, need_symlink_support,
)


# ───────────────────────── Fixtures ────────────────────────────────────────


def fx_empty() -> Fixture:
    return Fixture(name="empty", files=[])


def fx_single_small() -> Fixture:
    return Fixture(
        name="single_small",
        files=[FileSpec("hello.txt", content=b"hello world\n", mode=0o644)],
    )


def fx_text_files() -> Fixture:
    return Fixture(
        name="text_files",
        files=[
            FileSpec("readme.md", content=b"# project\n\nhello\n"),
            FileSpec("LICENSE", content=b"MIT\n"),
            FileSpec(".gitignore", content=b"target/\n*.log\n"),
        ],
    )


def fx_nested_tree() -> Fixture:
    return Fixture(
        name="nested_tree",
        files=[
            FileSpec("src/main.c", content=b"int main(){return 0;}\n"),
            FileSpec("src/lib/util.c", content=b"void util(){}\n"),
            FileSpec("src/lib/util.h", content=b"void util(void);\n"),
            FileSpec("docs/intro.md", content=b"intro\n"),
            FileSpec("docs/api/index.md", content=b"# API\n"),
            FileSpec("Makefile", content=b"all:\n\tcc src/main.c\n"),
        ],
    )


def fx_many_small() -> Fixture:
    return Fixture(
        name="many_small",
        files=[FileSpec(f"dir{i // 10}/file_{i:04d}.bin", content=f"#{i}\n".encode())
               for i in range(64)],
    )


def fx_large_binary() -> Fixture:
    """One file larger than the rsync default block (700 B for 64 KB) and
    larger than the 64 KB multiplex frame to exercise multi-frame transfer."""
    return Fixture(
        name="large_binary",
        files=[FileSpec("payload.bin", size=512 * 1024)],
    )


def fx_huge_binary() -> Fixture:
    return Fixture(
        name="huge_binary",
        files=[FileSpec("blob.bin", size=4 * 1024 * 1024)],
    )


def fx_mixed_sizes() -> Fixture:
    return Fixture(
        name="mixed_sizes",
        files=[
            FileSpec("zero.bin", content=b""),
            FileSpec("tiny.txt", content=b"x"),
            FileSpec("small.bin", size=3000),
            FileSpec("medium.bin", size=128 * 1024),
            FileSpec("large.bin", size=1024 * 1024),
        ],
    )


def fx_with_symlink() -> Fixture:
    return Fixture(
        name="with_symlink",
        files=[
            FileSpec("real.txt", content=b"real\n"),
            FileSpec("alias.txt", symlink_target="real.txt"),
        ],
    )


def fx_modes() -> Fixture:
    """Files with diverse permission bits."""
    return Fixture(
        name="modes",
        files=[
            FileSpec("readonly.txt", content=b"ro\n", mode=0o400),
            FileSpec("script.sh", content=b"#!/bin/sh\necho hi\n", mode=0o755),
            FileSpec("private.key", content=b"secret\n", mode=0o600),
            FileSpec("normal.txt", content=b"n\n", mode=0o644),
        ],
    )


def fx_special_names() -> Fixture:
    return Fixture(
        name="special_names",
        files=[
            FileSpec("file with spaces.txt", content=b"sp\n"),
            FileSpec("naïve-utf8-名字.txt", content=b"utf\n"),
            FileSpec("dash-and_underscore.dat", content=b"d\n"),
            FileSpec("dotfile/.hidden", content=b"hide\n"),
        ],
    )


def fx_hardlinks() -> Fixture:
    """Three files, two of which share an inode (hardlinked)."""
    return Fixture(
        name="hardlinks",
        files=[
            FileSpec("orig.txt", content=b"linked content\n"),
            FileSpec("link1.txt", hardlink_to="orig.txt"),
            FileSpec("link2.txt", hardlink_to="orig.txt"),
            FileSpec("alone.txt", content=b"alone\n"),
        ],
    )


# ───────────────────────── Scenario builders ───────────────────────────────


def _both(*preds):
    """Return a predicate that returns the first non-None skip reason."""
    def go():
        for p in preds:
            r = p()
            if r is not None:
                return r
        return None
    return go


def _local_only(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
    """rsync-rs running entirely on the local machine."""
    kw.setdefault("skip_if", need_binary("rsync-rs"))
    return Scenario(name=name, fixture=fx, sync=make_sync_local(flags), flags=flags, **kw)


def _c_pulls(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
    """C client pulls from rsync-rs (rsync-rs is the server-sender)."""
    kw.setdefault("skip_if", _both(need_binary("rsync"), need_binary("rsync-rs")))
    return Scenario(name=name, fixture=fx, sync=make_sync_pull_via_wrapper(flags), flags=flags, **kw)


def _c_pushes(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
    """C client pushes into rsync-rs (rsync-rs is the server-receiver)."""
    kw.setdefault("skip_if", _both(need_binary("rsync"), need_binary("rsync-rs")))
    return Scenario(name=name, fixture=fx, sync=make_sync_push_via_wrapper(flags), flags=flags, **kw)


def _rs_pulls_c(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
    """rsync-rs client pulls from a C server."""
    kw.setdefault("skip_if", _both(need_binary("rsync"), need_binary("rsync-rs")))
    return Scenario(name=name, fixture=fx, sync=make_sync_rs_pulls_from_c(flags), flags=flags, **kw)


def _rs_pushes_c(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
    """rsync-rs client pushes to a C server."""
    kw.setdefault("skip_if", _both(need_binary("rsync"), need_binary("rsync-rs")))
    return Scenario(name=name, fixture=fx, sync=make_sync_rs_push_to_c(flags), flags=flags, **kw)


def _self(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
    kw.setdefault("skip_if", need_binary("rsync-rs"))
    return Scenario(name=name, fixture=fx, sync=make_sync_self(flags), flags=flags, **kw)


# ───────────────────────── Scenario catalogue ──────────────────────────────


def all_scenarios() -> list[Scenario]:
    """Return the curated regression matrix.

    Naming convention: ``<mode>__<fixture>__<flags>`` so failures are easy to
    bucket.  ``mode`` is one of {local, c2rs, rs2c, c→rs, rs→c, self}.
    """
    sc: list[Scenario] = []

    # ── 1. Local mode (rsync-rs only) ─────────────────────────────────────
    for fx in (fx_single_small(), fx_text_files(), fx_nested_tree(),
               fx_many_small(), fx_mixed_sizes()):
        sc.append(_local_only(f"local__{fx.name}__a", fx, ["-a"]))
        sc.append(_local_only(f"local__{fx.name}__rt", fx, ["-rt"]))

    sc.append(_local_only("local__empty__a", fx_empty(), ["-a"]))

    sc.append(_local_only("local__with_symlink__a", fx_with_symlink(), ["-a"],
                          skip_if=need_symlink_support))

    # Permissions
    sc.append(_local_only("local__modes__a", fx_modes(), ["-a"]))
    sc.append(_local_only("local__modes__rt_no_perms", fx_modes(), ["-rt"],
                          ignore_mode=True))

    # Large content
    sc.append(_local_only("local__large_binary__a", fx_large_binary(), ["-a"], timeout_s=5))

    # Special names
    sc.append(_local_only("local__special_names__a", fx_special_names(), ["-a"]))

    # Hardlinks: with -aH, dst entries that share an inode in src must share
    # an inode in dst as well.
    def verify_hardlinks(dst: Path) -> "str | None":
        try:
            i_orig = (dst / "orig.txt").stat().st_ino
            i_l1   = (dst / "link1.txt").stat().st_ino
            i_l2   = (dst / "link2.txt").stat().st_ino
            i_alone = (dst / "alone.txt").stat().st_ino
        except OSError as e:
            return f"stat error: {e}"
        if not (i_orig == i_l1 == i_l2):
            return f"inodes differ: orig={i_orig} l1={i_l1} l2={i_l2}"
        if i_alone == i_orig:
            return "alone.txt unexpectedly shares inode with orig.txt"
        return None

    sc.append(_local_only("local__hardlinks__aH", fx_hardlinks(), ["-aH"],
                          verify_dst=verify_hardlinks))

    # Hardlinks via protocol (C↔rsync-rs): verify inode sharing is preserved.
    sc.append(_c_pulls("c_pulls__hardlinks__aH", fx_hardlinks(), ["-aH"],
                       verify_dst=verify_hardlinks))
    sc.append(_c_pushes("c_pushes__hardlinks__aH", fx_hardlinks(), ["-aH"],
                        verify_dst=verify_hardlinks))

    # ── 2. C client pulls from rsync-rs (server-sender path) ──────────────
    for fx in (fx_single_small(), fx_text_files(), fx_nested_tree(), fx_mixed_sizes()):
        sc.append(_c_pulls(f"c_pulls__{fx.name}__vrt", fx, ["-vrt"]))
        sc.append(_c_pulls(f"c_pulls__{fx.name}__av", fx, ["-av"]))

    sc.append(_c_pulls("c_pulls__large_binary__av", fx_large_binary(), ["-av"], timeout_s=5))
    sc.append(_c_pulls("c_pulls__many_small__av", fx_many_small(), ["-av"], timeout_s=5))
    sc.append(_c_pulls("c_pulls__with_symlink__av", fx_with_symlink(), ["-av"],
                       skip_if=need_symlink_support))

    # Compression — implemented via DeflatedTokenWriter/Reader (token.c port).
    sc.append(_c_pulls("c_pulls__mixed__avz", fx_mixed_sizes(), ["-avz"], timeout_s=5))

    # Delta (pre-populate destination with a near-copy)
    def setup_delta(dst: Path) -> None:
        # Plant a slightly modified version of payload.bin so the sender has
        # to do real block matching.
        target = dst / "payload.bin"
        target.parent.mkdir(parents=True, exist_ok=True)
        # Reproducible "almost" content: same size, half the bytes flipped.
        import hashlib
        seed = b"large_binary:payload.bin"
        h = hashlib.sha256(seed).digest()
        buf = bytearray()
        while len(buf) < 512 * 1024:
            buf.extend(h)
            h = hashlib.sha256(h).digest()
        for i in range(0, len(buf), 4096):
            buf[i] ^= 0xFF
        target.write_bytes(bytes(buf[:512 * 1024]))
        # Backdate the dst mtime so C's quick-check (size+mtime) does NOT skip
        # the transfer. We need the file to look "outdated" relative to source.
        import os
        old = target.stat().st_mtime - 3600
        os.utime(target, (old, old))
    sc.append(_c_pulls("c_pulls__delta_large__av", fx_large_binary(), ["-av"],
                       setup_dst=setup_delta, timeout_s=5))

    # --inplace: receiver writes directly to dest (no temp+rename).
    sc.append(_c_pulls("c_pulls__inplace__av", fx_text_files(), ["-av", "--inplace"]))

    # --itemize-changes: receiver prints itemize lines for each file.
    sc.append(_c_pulls("c_pulls__itemize__a", fx_text_files(), ["-a", "--itemize-changes"]))

    # ── 3. C client pushes to rsync-rs (server-receiver path) ─────────────
    for fx in (fx_single_small(), fx_text_files(), fx_nested_tree(), fx_mixed_sizes()):
        sc.append(_c_pushes(f"c_pushes__{fx.name}__av", fx, ["-av"]))

    # ── 4. rsync-rs client pulls from C server ────────────────────────────
    for fx in (fx_single_small(), fx_text_files(), fx_nested_tree(), fx_mixed_sizes()):
        sc.append(_rs_pulls_c(f"rs_pulls_c__{fx.name}__av", fx, ["-av"]))

    # ── 4b. rsync-rs client pushes to C server (the new direction) ────────
    for fx in (fx_single_small(), fx_text_files(), fx_nested_tree(), fx_mixed_sizes()):
        sc.append(_rs_pushes_c(f"rs_pushes_c__{fx.name}__av", fx, ["-av"]))

    # ── 5. rsync-rs ↔ rsync-rs (self) ────────────────────────────────────
    for fx in (fx_single_small(), fx_text_files(), fx_nested_tree(), fx_mixed_sizes()):
        sc.append(_self(f"self__{fx.name}__av", fx, ["-av"]))

    # ── 6. --delete support ───────────────────────────────────────────────
    def _make_setup_with_stale(stale_names: list[str]):
        """Return a setup_dst that plants stale files that --delete should remove."""
        def setup(dst: Path) -> None:
            for name in stale_names:
                p = dst / name
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_bytes(b"STALE_CONTENT")
        return setup

    def _make_verify_deleted(stale_names: list[str]):
        """Return a verify_dst that checks stale files are gone."""
        def verify(dst: Path) -> "str | None":
            survivors = [n for n in stale_names if (dst / n).exists()]
            if survivors:
                return f"stale files still exist after --delete: {survivors}"
            return None
        return verify

    _stale = ["stale.txt", "old/unwanted.bin"]
    _setup_stale = _make_setup_with_stale(_stale)
    _verify_stale = _make_verify_deleted(_stale)

    # local --delete (already supported, regression guard)
    sc.append(_local_only("local__delete__av", fx_text_files(), ["-av", "--delete"],
                          setup_dst=_setup_stale, verify_dst=_verify_stale))

    # rsync-rs server-sender, C client pulls with --delete
    sc.append(_c_pulls("c_pulls__delete__av", fx_text_files(), ["-av", "--delete"],
                       setup_dst=_setup_stale, verify_dst=_verify_stale))

    # rsync-rs client pulls from C server with --delete
    sc.append(_rs_pulls_c("rs_pulls_c__delete__av", fx_text_files(), ["-av", "--delete"],
                          setup_dst=_setup_stale, verify_dst=_verify_stale))

    # rsync-rs client pushes to C server with --delete
    sc.append(_rs_pushes_c("rs_pushes_c__delete__av", fx_text_files(), ["-av", "--delete"],
                           setup_dst=_setup_stale, verify_dst=_verify_stale))

    # rsync-rs ↔ rsync-rs (self) with --delete
    sc.append(_self("self__delete__av", fx_text_files(), ["-av", "--delete"],
                    setup_dst=_setup_stale, verify_dst=_verify_stale))

    # ── 7. --exclude / --include filter support ───────────────────────────
    # Fixture: files with varied extensions; we exclude *.log and build/ dir.
    fx_excl = Fixture(
        name="exclude_test",
        files=[
            FileSpec("keep.txt", content=b"keep\n"),
            FileSpec("notes.log", content=b"log data\n"),
            FileSpec("src/main.c", content=b"int main(){}\n"),
            FileSpec("src/debug.log", content=b"debug\n"),
            FileSpec("build/output.bin", content=b"compiled\n"),
            FileSpec("README.md", content=b"readme\n"),
        ],
    )
    _excl_flags = ["-av", "--exclude=*.log", "--exclude=build/"]
    _excl_present = ["keep.txt", "src/main.c", "README.md"]
    _excl_absent  = ["notes.log", "src/debug.log", "build/output.bin"]

    def _verify_exclude(dst: Path) -> "str | None":
        missing = [n for n in _excl_present if not (dst / n).exists()]
        present = [n for n in _excl_absent  if (dst / n).exists()]
        errs = []
        if missing:
            errs.append(f"expected files not synced: {missing}")
        if present:
            errs.append(f"excluded files were synced anyway: {present}")
        return "; ".join(errs) if errs else None

    sc.append(_local_only("local__exclude__av", fx_excl, _excl_flags,
                          ignore_paths=_excl_absent, verify_dst=_verify_exclude))
    sc.append(_c_pulls("c_pulls__exclude__av", fx_excl, _excl_flags,
                       ignore_paths=_excl_absent, verify_dst=_verify_exclude))
    sc.append(_rs_pulls_c("rs_pulls_c__exclude__av", fx_excl, _excl_flags,
                          ignore_paths=_excl_absent, verify_dst=_verify_exclude))
    sc.append(_rs_pushes_c("rs_pushes_c__exclude__av", fx_excl, _excl_flags,
                           ignore_paths=_excl_absent, verify_dst=_verify_exclude))

    # ── 8. --checksum: force content comparison rather than size+mtime ────
    # Plant a same-size file with DIFFERENT content but a future mtime so a pure
    # size+mtime check would call it up-to-date; --checksum must force re-transfer.
    # fx_text_files() has readme.md = b"# project\n\nhello\n" (17 bytes).
    _CHECKSUM_TRAP = b"WRONG WRONG WRONG"  # exactly 17 bytes, wrong content
    assert len(_CHECKSUM_TRAP) == 17, "trap size must match source readme.md size"

    def _setup_checksum_trap(dst: Path) -> None:
        p = dst / "readme.md"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(_CHECKSUM_TRAP)
        import os, time
        # Future mtime so a size+mtime check would (incorrectly) say "up to date"
        future = time.time() + 3600
        os.utime(p, (future, future))

    def _verify_checksum(dst: Path) -> "str | None":
        p = dst / "readme.md"
        if not p.exists():
            return "readme.md missing from dst"
        got = p.read_bytes()
        if got == _CHECKSUM_TRAP:
            return "--checksum did not re-transfer the modified file"
        return None

    sc.append(_local_only("local__checksum__a", fx_text_files(), ["-a", "--checksum"],
                          setup_dst=_setup_checksum_trap, verify_dst=_verify_checksum))
    # c_pulls__checksum__av: C rsync client with --checksum pulls from rsync-rs server.
    # The bundled 'c' flag in the server args must set checksum_len so the file list
    # includes per-file checksums; otherwise the wire format is mismatched.
    sc.append(_c_pulls("c_pulls__checksum__av", fx_text_files(), ["-av", "--checksum"],
                       setup_dst=_setup_checksum_trap, verify_dst=_verify_checksum))
    # c_pushes__checksum__av: C rsync client with --checksum pushes to rsync-rs server.
    sc.append(_c_pushes("c_pushes__checksum__av", fx_text_files(), ["-av", "--checksum"],
                        setup_dst=_setup_checksum_trap, verify_dst=_verify_checksum))
    # rs_pulls_c__checksum__av: rsync-rs client with --checksum pulls from C server.
    sc.append(_rs_pulls_c("rs_pulls_c__checksum__av", fx_text_files(), ["-av", "--checksum"],
                          setup_dst=_setup_checksum_trap, verify_dst=_verify_checksum))
    # rs_pushes_c__checksum__av: rsync-rs client with --checksum pushes to C server.
    sc.append(_rs_pushes_c("rs_pushes_c__checksum__av", fx_text_files(), ["-av", "--checksum"],
                           setup_dst=_setup_checksum_trap, verify_dst=_verify_checksum))
    # self__checksum__av: rsync-rs client with --checksum to rsync-rs server.
    sc.append(_self("self__checksum__av", fx_text_files(), ["-av", "--checksum"],
                    setup_dst=_setup_checksum_trap, verify_dst=_verify_checksum))

    # ── 9. --write-batch / --read-batch (rsync-rs native format) ─────────────
    # Each test: write-batch from src, apply read-batch to dst, compare trees.
    def _batch(name: str, fx: Fixture, flags: list[str], **kw) -> Scenario:
        kw.setdefault("skip_if", need_binary("rsync-rs"))
        return Scenario(name=name, fixture=fx, sync=make_sync_batch(flags), flags=flags, **kw)

    sc.append(_batch("batch__single_small__av",  fx_single_small(),  ["-av"]))
    sc.append(_batch("batch__nested_tree__av",   fx_nested_tree(),   ["-av"]))
    sc.append(_batch("batch__text_files__av",    fx_text_files(),    ["-av"]))
    sc.append(_batch("batch__mixed_sizes__av",   fx_mixed_sizes(),   ["-av"]))

    # ── 10. --backup (in-place suffix) and --backup-dir ───────────────────────
    def _setup_existing(dst: Path) -> None:
        """Pre-populate dst with a file that will be overwritten."""
        f = dst / "readme.md"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"OLD CONTENT\n")

    def _verify_backup_suffix(dst: Path) -> "str | None":
        orig = dst / "readme.md"
        bak  = dst / "readme.md~"
        if not orig.exists():
            return "readme.md missing after --backup sync"
        if not bak.exists():
            return "readme.md~ backup not created"
        if bak.read_bytes() != b"OLD CONTENT\n":
            return "backup file has wrong content"
        return None

    sc.append(_local_only("local__backup_suffix__av",
                          fx_text_files(), ["-av", "--backup"],
                          setup_dst=_setup_existing, verify_dst=_verify_backup_suffix,
                          ignore_paths=["readme.md~"]))

    def _verify_backup_dir_result(dst: Path) -> "str | None":
        bak = dst.parent / "bak"
        bak_file = bak / "readme.md"
        if not bak_file.exists():
            return f"backup-dir file not created: {bak_file}"
        if bak_file.read_bytes() != b"OLD CONTENT\n":
            return "backup-dir file has wrong content"
        return None

    sc.append(Scenario(
        name="local__backup_dir__av",
        fixture=fx_text_files(),
        sync=make_sync_backup_dir(["-av", "--backup"], backup_subdir="bak"),
        flags=["-av", "--backup"],
        skip_if=need_binary("rsync-rs"),
        setup_dst=_setup_existing,
        verify_dst=_verify_backup_dir_result,
    ))

    return sc


def smoke_scenarios() -> list[Scenario]:
    """A small, fast subset suitable for pre-commit hooks (~5 seconds)."""
    return [
        _local_only("local__single_small__a", fx_single_small(), ["-a"]),
        _local_only("local__nested_tree__a", fx_nested_tree(), ["-a"]),
        _c_pulls("c_pulls__single_small__vrt", fx_single_small(), ["-vrt"]),
        _c_pulls("c_pulls__single_small__av", fx_single_small(), ["-av"]),
    ]


def filter_scenarios(scenarios: Iterable[Scenario], pattern: str) -> list[Scenario]:
    import re
    rx = re.compile(pattern)
    return [s for s in scenarios if rx.search(s.name)]
