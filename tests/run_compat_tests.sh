#!/bin/bash
set -euo pipefail

RSYNC_C="${RSYNC_C_BIN:-rsync}"
RSYNC_RS="${RSYNC_RUST_BIN:-rsync-rs}"
PASS=0
FAIL=0

log()  { echo "[TEST] $*"; }
pass() { log "PASS: $1"; PASS=$((PASS+1)); }
fail() { log "FAIL: $1"; FAIL=$((FAIL+1)); }

setup_test_data() {
    local dir="$1"
    mkdir -p "$dir/subdir"
    echo "Hello, World!" > "$dir/hello.txt"
    echo "Test file 2" > "$dir/test2.txt"
    dd if=/dev/urandom bs=1024 count=100 of="$dir/random_100k.bin" 2>/dev/null
    dd if=/dev/urandom bs=1024 count=1024 of="$dir/large_file.bin" 2>/dev/null
    echo "subdir file" > "$dir/subdir/nested.txt"
    ln -sf ../hello.txt "$dir/subdir/link_to_hello" 2>/dev/null || true
}

compare_dirs() {
    local src="$1" dst="$2" label="$3"
    if diff -rq --no-dereference "$src" "$dst" > /dev/null 2>&1; then
        pass "$label: directories match"
    else
        fail "$label: directories DIFFER"
        diff -rq --no-dereference "$src" "$dst" || true
    fi
}

# Test 1: C→C local (baseline)
test_cc_local() {
    log "Test 1: C rsync local sync (baseline)"
    local src="/tmp/t1src" dst="/tmp/t1dst"
    rm -rf "$src" "$dst"
    setup_test_data "$src"
    "$RSYNC_C" -a "$src/" "$dst/"
    compare_dirs "$src" "$dst" "C→C local"
    rm -rf "$src" "$dst"
}

# Test 2: Rust→Rust local
test_rr_local() {
    log "Test 2: Rust rsync local sync"
    local src="/tmp/t2src" dst="/tmp/t2dst"
    rm -rf "$src" "$dst"
    setup_test_data "$src"
    if "$RSYNC_RS" -a "$src/" "$dst/" 2>/dev/null; then
        compare_dirs "$src" "$dst" "Rust→Rust local"
    else
        fail "Rust→Rust local: rsync-rs exited with error (expected in early dev)"
    fi
    rm -rf "$src" "$dst"
}

# Test 3: C sender, Rust receiver
test_c_sender_rust_receiver() {
    log "Test 3: C sender → Rust receiver"
    local src="/tmp/t3src" dst="/tmp/t3dst"
    rm -rf "$src" "$dst"
    setup_test_data "$src"
    # local protocol: C rsync calls rsync-rs --server
    if "$RSYNC_C" -a --rsync-path="$RSYNC_RS" "$src/" "localhost::test" 2>/dev/null; then
        compare_dirs "$src" "$dst" "C→Rust"
    else
        fail "C→Rust: not yet implemented (expected)"
    fi
    rm -rf "$src" "$dst"
}

# Test 4: Rust sender, C receiver
test_rust_sender_c_receiver() {
    log "Test 4: Rust sender → C receiver"
    local src="/tmp/t4src" dst="/tmp/t4dst"
    rm -rf "$src" "$dst"
    setup_test_data "$src"
    if "$RSYNC_RS" -a --rsync-path="$RSYNC_C" "$src/" "localhost::test" 2>/dev/null; then
        compare_dirs "$src" "$dst" "Rust→C"
    else
        fail "Rust→C: not yet implemented (expected)"
    fi
    rm -rf "$src" "$dst"
}

# Test 5: Delta (incremental) transfer
test_delta_transfer() {
    log "Test 5: Delta (incremental) transfer"
    local src="/tmp/t5src" dst="/tmp/t5dst"
    rm -rf "$src" "$dst"
    setup_test_data "$src"
    "$RSYNC_C" -a "$src/" "$dst/"
    echo "modification line" >> "$src/hello.txt"
    dd if=/dev/urandom bs=1024 count=10 >> "$src/large_file.bin" 2>/dev/null
    if "$RSYNC_RS" -a "$src/" "$dst/" 2>/dev/null; then
        compare_dirs "$src" "$dst" "Delta/incremental"
    else
        fail "Delta: rsync-rs failed (expected in early dev)"
    fi
    rm -rf "$src" "$dst"
}

test_cc_local
test_rr_local
test_c_sender_rust_receiver
test_rust_sender_c_receiver
test_delta_transfer

echo ""
echo "====================================="
echo "Results: $PASS passed, $FAIL failed"
echo "====================================="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
