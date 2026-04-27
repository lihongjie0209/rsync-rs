#!/usr/bin/env bash
#
# tests/run_regression.sh
#
# Top-level driver for the rsync-rs regression suite.  Builds the binary,
# spins up the docker test image, and runs the Python harness inside.  The
# script auto-detects whether docker is available; without docker it falls
# back to running directly on the host (useful on Linux dev boxes that
# already have C rsync installed).
#
# Usage:
#     tests/run_regression.sh              # full matrix in docker
#     tests/run_regression.sh --smoke      # fast subset
#     tests/run_regression.sh --host       # skip docker, run on host
#     tests/run_regression.sh -k symlink   # filter by regex
#

set -Eeuo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)

USE_DOCKER=auto
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --host)   USE_DOCKER=no; shift ;;
        --docker) USE_DOCKER=yes; shift ;;
        *)        ARGS+=("$1"); shift ;;
    esac
done

if [[ "$USE_DOCKER" == "auto" ]]; then
    if command -v docker >/dev/null 2>&1; then USE_DOCKER=yes; else USE_DOCKER=no; fi
fi

# ── Build ─────────────────────────────────────────────────────────────────
echo "→ cargo build --release"
cargo build --release --quiet

if [[ "$USE_DOCKER" == "yes" ]]; then
    echo "→ docker build -t rsync-rs-test ."
    docker build -q -t rsync-rs-test . >/dev/null

    echo "→ running suite inside container"
    exec docker run --rm \
        -v "$ROOT:/workspace:ro" \
        -w /workspace \
        rsync-rs-test \
        python3 -m tests.regress "${ARGS[@]}"
else
    if ! command -v rsync >/dev/null 2>&1; then
        echo "WARN: C rsync not installed; client/server scenarios will skip" >&2
    fi
    if ! command -v rsync-rs >/dev/null 2>&1; then
        # Use the freshly-built binary.
        export PATH="$ROOT/target/release:$PATH"
    fi
    if [[ ! -x /usr/local/bin/wrapper ]]; then
        TMP=$(mktemp)
        cat >"$TMP" <<'EOF'
#!/bin/sh
shift 2
exec rsync-rs "$@"
EOF
        chmod +x "$TMP"
        export RSYNC_WRAPPER="$TMP"
    fi
    exec python3 -m tests.regress "${ARGS[@]}"
fi
