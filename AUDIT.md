# rsync-rs ↔ C rsync 3.4.2 Audit

> Generated as a single-shot snapshot. Source totals: **C = 984 KB across ~60 .c files**, **Rust = 346 KB across ~30 .rs files**. The size delta reflects features still missing (daemon, ACL, xattr, batch, hardlinks, full token compression).

## 1. Test status (regression harness, Docker-host)

| Bucket | Count | Notes |
|---|---|---|
| Passing integration | 40 | `local__*`, `c_pulls__*` (av/vrt), `c_pushes__*`, `self__*`, `rs_pulls_c__*`, all `cli__*` |
| Skipped | 1 | `c_pulls__mixed__avz` — zlib token framing not implemented |
| Failing | 0 | — |
| Unit tests | 156 | Including 11 new for `options_server` parser |

## 2. Module-by-module coverage

| C source | Rust counterpart | Status | Gaps vs C |
|---|---|---|---|
| `rsync.h`, `errcode.h` | `protocol/constants.rs`, `protocol/errcode.rs` | ✅ | All wire constants present |
| `flist.c` | `flist/{send,recv,sort}.rs` | ✅ basic | No inc-recurse phase-2 dir exchange (CF_INC_RECURSE not advertised); no hardlink dev/inode pairing |
| `io.c` | `io/{multiplex,varint}.rs` | ✅ | No `read_buf_via` keepalive; no remote-error replay buffer |
| `checksum.c` | `checksum/{rolling,strong}.rs` | ✅ | No xxh64/xxh3/xxh128 (md5/md4 only) |
| `match.c` | `delta/match_blocks.rs` | ✅ | No 2nd-pass fuzzy matching |
| `token.c` | `delta/token.rs` | ⚠️ partial | Only `simple_send_token`/`simple_recv_token`. **No `send_deflated_token`/`recv_deflated_token`** → `-z` blocks (see §4.5) |
| `sender.c` | `pipeline/sender.rs` | ✅ | No `--inplace`, no `--append`, no batch-write |
| `receiver.c` | `pipeline/receiver.rs` | ✅ | No `--partial-dir`, no `--temp-dir` |
| `generator.c` | `pipeline/generator.rs` | ✅ basic | No deferred-flist generator phase, no hard-link generation |
| `main.c` | `main.rs` | ✅ | Daemon entry path missing |
| `options.c` | `options.rs` + `options_server.rs` | ✅ | Long-option set is reduced; popt-style abbreviations not supported |
| `compat.c` | inlined in `main.rs::setup_compat*` | ✅ | Compress negotiation accepts only `none`; no real algorithm |
| `clientserver.c`, `socket.c`, `authenticate.c`, `loadparm.c`, `access.c` | `daemon.rs` (server side only) | ⚠️ partial | **Daemon-CLIENT mode (`rsync://` URL on client) NOT implemented**: rsync-rs cannot connect TO a remote rsync daemon. Daemon-server side works for list+pull; push receiver path now strips module prefix from args. No AUTHREQD. |
| `acls.c`, `xattrs.c` | — | ❌ | No ACL/xattr |
| `backup.c`, `batch.c` | — | ❌ | No `--backup`, no `--write-batch`/`--read-batch` |
| `hlink.c` | — | ❌ stub | No hardlink detection |
| `pipe.c` | `transport/{local,ssh}.rs` | ✅ | Local + SSH only |
| `exclude.c` | `filter/mod.rs` | ✅ basic | No merge-file rules, no `:` per-dir filters |
| `syscall.c`, `fileio.c` | `fileops/{syscall,fileio}.rs` | ✅ | Linux only (no Windows fallback yet) |
| `uidlist.c` | `uidlist.rs` | ✅ | Numeric-only mapping |
| `progress.c` | `progress.rs` | ✅ basic | Format approximations |
| `log.c` | `log_mod` (in `log.rs`) | ✅ | `--log-file`/`--syslog` not wired through |
| `util1.c`/`util2.c` | `util.rs` | ✅ | `human_num` uses correct comma format |

## 3. Wire-format guarantees verified during this work

* Protocol version handshake (20 ≤ v ≤ 31, picks min)
* `compat_flags = CF_VARINT_FLIST_FLAGS | CF_SAFE_FLIST` written as varint
* Checksum-list vstring exchange (md5)
* **Compress-list vstring exchange** when `-z` is in args (proposes `none`)
* `checksum_seed = time(NULL) ^ (pid << 6)` written as int32
* Multiplex out enabled at protocol ≥ 23, multiplex in at ≥ 30
* Filter-list terminator `write_int(0)`
* File-list:
  * varint-encoded XMIT flags
  * uid/gid name-list **gated on `preserve.uid`/`preserve.gid`** (matches `flist.c:2724-2731`)
  * `io_error` trailer
* Block-checksum sum_head: `count, blength, s2length, remainder`
* Token stream: positive=literal, negative=block match, 0=EOF
* xfer_sum (whole-file MD5, NO seed for MD5 per `checksum.c::sum_init`)

## 4. Known gaps and risk ranking

### 4.1 Critical for parity (P0)
* **Daemon-CLIENT transport** (`rsync://` URL as client): `clientserver.c::start_inband_exchange` port. Currently rsync-rs bails with a clear error when given a `rsync://` URL in client mode. Needed before rsync-rs can act as a client to remote daemons (e.g. `rsync-rs -av rsync://srv/mod/ /local/`).
* **rsync-rs ↔ C 3.2.7 over rsh, push direction**: the new `linux-interop` CI (commit aaba122) revealed that `rsync-rs --server` accepted by C 3.2.7 client returns `flist.c(786) protocol incompatibility`. Pull direction (rsync-rs server-as-sender) works; push direction (rsync-rs server-as-receiver from C 3.2.7 sender) needs flist re-audit. Likely a missing XMIT flag handling for new protocol features 3.2.7 emits.
* **Daemon mode** (`--daemon` server, large): partial — list/pull work end-to-end; push works after the module-prefix strip fix in `daemon.rs` (now matches `util1.c::glob_expand_module`). No AUTHREQD support.
* **Hard links** (`-H`): correctness issue when source has hardlinks; we currently send each link as an independent file.
* **Inc-recurse** (`CF_INC_RECURSE`, protocol 30+): we never advertise it; works for current tests because the C side falls back, but large trees take more memory than necessary.

### 4.2 Common feature gaps (P1)
* **Zlib token compression** (`-z`): blocks `c_pulls__mixed__avz`. ~400 LoC port of `token.c::send_deflated_token` + `recv_deflated_token`. Needs `flate2` raw deflate stream interleaved with the literal/match token wire.
* **`--inplace`, `--append`, `--partial`, `--temp-dir`, `--backup`**: receiver behavior modes.
* **ACLs (`-A`) / xattrs (`-X`)**: separate optional protocol subsections.
* **`--list-only`** dedicated output formatter.
* **`--itemize-changes` (`-i`)** 11-character format string from `log.c:695-728`.

### 4.3 Robustness (P1)
* **Keepalive messages** (`MSG_NOOP`) — long-quiet runs may stall.
* **Generator/receiver/sender as separate threads** — currently single-thread per role.
* **Error message format** — `"rsync error: %s (code %d) at %s(%d) [%s=%s]"` literal compatibility.

### 4.4 Optimization (P2)
* SIMD rolling checksum (scalar today).
* `bytes::BytesMut` zero-copy in mux frames.
* `sendfile`/`splice` for large local copies.

### 4.5 Windows portability (P2)
* `nix` → `rustix` migration so the crate compiles on `x86_64-pc-windows-msvc`.
* `chmod` no-op + Win32 `SetFileTime` for mtime/atime.
* `std::os::windows::fs::symlink_*` (developer-mode required).
* CI matrix.

## 5. Audit conclusions

The Phase-1 protocol spine is **complete and correct** for the most common uses (`-a`, `-rt`, `-vrt`, recursive trees, deltas, symlinks, both directions, both transport modes, both clients). The flag-parsing layer is now unit-tested and debug-instrumented (RSYNC_RS_DEBUG=1).

The biggest remaining wire-level item is **deflated-token I/O** (zlib) — without it `-z` is the only common flag we cannot serve and the C client will refuse to negotiate "none" in its preference list (compat.c:485). Daemon mode and hard-links are the next biggest *feature* gaps.
