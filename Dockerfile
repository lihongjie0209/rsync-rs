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
# Wrapper: when C rsync invokes "rsync" on the remote, forward to rsync-rs.
# rsync calls: wrapper <remote-host> rsync <server-args...>
# We skip the first two positional args (host + "rsync") and exec rsync-rs with the rest.
RUN printf '#!/bin/sh\nshift 2\nexec /usr/local/bin/rsync-rs "$@"\n' > /usr/local/bin/wrapper \
    && chmod +x /usr/local/bin/wrapper
RUN rsync --version
WORKDIR /test
CMD ["/bin/bash"]
