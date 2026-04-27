# rsync-rs

A Rust implementation of [rsync](https://rsync.samba.org/), aiming for full
wire-protocol compatibility with the upstream C `rsync` (3.4.x) so that a
Rust client/server can talk to a C server/client and vice versa.

## Status

- **Local mode** (`rsync-rs SRC DST`): regular files, symlinks, permissions,
  mtimes, hard links (`-H`), zlib token framing (`--compress`), in-place
  updates (`--inplace`), file-list verbose output, itemize-changes (`-i`).
- **Remote mode** over SSH-style stdin/stdout: client and server, push and
  pull, protocol 31, multiplexed I/O, checksum negotiation.
- **Daemon mode** (`--daemon`): anonymous `rsyncd` with `[module]` config,
  `@RSYNCD:` greeting, module listing, fork-per-connection (Unix).
- **xattrs** (`--xattrs`): preserved in local-mode (best-effort, Unix only).
- **ACLs** (`--acls`): currently a no-op with a startup warning.

Regression suite: 49/49 scenarios passing against the C reference, including
C ↔ Rust bidirectional transfers.

## Building

```
cargo build --release
```

The binary lands at `target/release/rsync-rs`.

## License

GPL-3.0, matching upstream rsync.
