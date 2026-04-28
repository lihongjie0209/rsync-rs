# Stage 1: Build Rust binary
FROM rust:1.86-bookworm AS rust-builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs && cargo build --release 2>/dev/null; exit 0
COPY src/ ./src/
RUN touch src/main.rs && cargo build --release

# Stage 2: Test image with both C rsync and Rust rsync
FROM debian:bookworm-slim AS test
RUN apt-get update && apt-get install -y rsync openssh-server openssh-client python3 ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=rust-builder /workspace/target/release/rsync-rs /usr/local/bin/rsync-rs
# Wrapper: a minimal stand-in for `ssh`/`rsh`.  rsync invokes us as
#   wrapper <remote-host> <remote-cmd> <server-args...>
# We drop the host and exec the requested remote-cmd.  This makes the same
# wrapper work for BOTH directions: when --rsync-path=rsync-rs the wrapper
# launches rsync-rs as the server, and when --rsync-path=rsync (or default)
# it launches C rsync — needed for full Rust↔C interop coverage.
RUN printf '#!/bin/sh\nshift 1\nexec "$@"\n' > /usr/local/bin/wrapper \
    && chmod +x /usr/local/bin/wrapper
RUN rsync --version
WORKDIR /test
CMD ["/bin/bash"]
