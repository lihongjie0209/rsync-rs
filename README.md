# rsync-rs

A Rust implementation of [rsync](https://rsync.samba.org/), aiming for full
wire-protocol compatibility with the upstream C `rsync` (3.4.x) so that a
Rust client/server can talk to a C server/client and vice versa.

## Feature implementation

Legend: ✅ implemented · ⚠️ partial · ❌ not yet

| Area | Feature | Status | Notes |
|---|---|---|---|
| Transport | Local mode (`SRC DST`) | ✅ | Files, dirs, symlinks, hardlinks, perms, mtimes |
| | Remote shell (`-e ssh`, `host:path`) | ✅ | Protocol 31, both directions |
| | Daemon **server** (`--daemon`, `rsyncd.conf`) | ✅ | `@RSYNCD:` greeting, modules, list, pull, push (read-only off), fork-per-connection on Unix |
| | Daemon **client** (`rsync://host/MOD/path`) | ✅ | Push and pull against C rsync 3.2.7 daemon verified |
| Wire format | Protocol versions 27–31, varint flist, MD5 strong sum, checksum-list negotiation | ✅ | |
| | Inc-recurse (`CF_INC_RECURSE`) | ❌ | Falls back to non-incremental flist |
| | Multiplexed I/O (`MSG_DATA/INFO/ERR`) | ✅ | |
| Files | Regular files, dirs, symlinks | ✅ | |
| | Hard links (`-H`) | ⚠️ | Local mode preserves; remote may send dupes |
| | Devices/specials (`-D`) | ✅ | |
| | xattrs (`-X`) | ⚠️ | Local mode only, Unix only |
| | ACLs (`-A`) | ❌ | Accepted as no-op with warning |
| Delta | Rolling+strong block matching | ✅ | |
| | Compression `-z` (zlib token) | ✅ | Negotiates `none` if peer disagrees |
| | `--inplace`, `--append`, `--partial` | ⚠️ | `--inplace` works; `--partial-dir`/`--temp-dir` not wired |
| | `--write-batch`, `--read-batch` | ✅ | rsync-rs native `RSYNBAT1` format; not C-compatible |
| | `--backup`, `--backup-dir`, `--suffix` | ✅ | In-place `~` suffix or separate backup-dir; local mode |
| Output | `-v`, `-vv`, `--progress`, `--stats` | ✅ | Stream/format match C rsync |
| | `--itemize-changes` (`-i`) | ✅ | 11-char format |
| | `--list-only` | ✅ | |
| | `--help`, `--version` | ✅ | Hand-rolled to mirror C layout |
| Filters | `--exclude`/`--include`, `--exclude-from` | ✅ basic | No merge-files (`:` per-dir) |
| Platforms | Linux x86_64, aarch64 (gnu+musl) | ✅ | CI build + tests |
| | macOS x86_64, aarch64 | ✅ | CI build + tests |
| | Windows x86_64 (MSVC) | ✅ | Local + self-loop daemon work; rsh transport over OpenSSH supported |

## Cross-platform & cross-implementation compatibility

CI runs the full regression suite (`tests/regress/`) on every push.  Each
cell is a real subprocess run against the platform's native peer, not a
mock.

| Direction | Local | rsh / SSH | Daemon (`rsync://`) |
|---|---|---|---|
| **rs ↔ rs** (rsync-rs ↔ rsync-rs) | ✅ Linux · ✅ macOS · ✅ Windows | ✅ Linux · ✅ Windows (OpenSSH) | ✅ Linux self-loop · ✅ Windows self-loop |
| **rs → C** (rs client → C server, push) | n/a | ✅ Linux (all rsh scenarios pass in CI) | ✅ Verified against C rsync 3.2.7 daemon (Docker) |
| **rs ← C** (rs client ← C server, pull) | n/a | ✅ Linux (all rsh scenarios pass in CI) | ✅ Verified against C rsync 3.2.7 daemon (Docker) |
| **C → rs** (C client → rs server, push) | n/a | ✅ Linux | ✅ Linux daemon receiver |
| **C ← rs** (C client ← rs server, pull) | n/a | ✅ Linux | ✅ Linux daemon sender, list, file pull |
| **rs (Windows) ↔ C (Linux)** | n/a | ✅ SSH push+pull verified (Windows OpenSSH → Docker C rsync) | ✅ Daemon push+pull verified (Windows → Docker C rsync 3.2.7) |

CI matrix per push (`.github/workflows/ci.yml`):

| Job | What it covers |
|---|---|
| `Unit tests` (Linux, macOS, Windows) | `cargo test` — 168+ unit tests |
| `Build *` (6 targets) | Release artifacts for Linux gnu/musl, macOS, Windows |
| `Windows smoke (native)` | rsync-rs ↔ rsync-rs on Windows via local + cwRsync rsh |
| `Windows smoke (interop)` | rsync-rs ↔ cwRsync (Cygwin C build) on Windows |
| `Linux interop` | 56 scenarios: local + rsh + daemon, rs↔rs and rs↔C 3.2.7 — **56/56 pass** |

### Known interop gaps

1. **No AUTHREQD support** in the daemon server — modules with
   `auth users`/`secrets file` are not honored.

## Building

```
cargo build --release
```

The binary lands at `target/release/rsync-rs`.

## License

GPL-3.0, matching upstream rsync.
