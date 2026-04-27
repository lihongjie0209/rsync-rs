//! Progress reporting mirroring progress.c from rsync.
//!
//! Prints a live "bytes transferred / total" progress line to stderr,
//! updated in place via carriage return, then a final summary line.

#![allow(dead_code)]

use std::cell::Cell;
use std::io::Write;
use std::time::Instant;

// ── Internal state ─────────────────────────────────────────────────────────
// All state is accessed from a single thread (rsync serialises transfers),
// so Cell<Option<Instant>> is sufficient and avoids static-mut-ref UB.

thread_local! {
    static PROGRESS_START: Cell<Option<Instant>> = Cell::new(None);
    static LAST_OFFSET: Cell<i64> = Cell::new(0);
    static LAST_ELAPSED_SECS: Cell<f64> = Cell::new(0.0);
}

fn elapsed_since_start() -> f64 {
    PROGRESS_START.with(|c| {
        c.get().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0)
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Print (or update) a progress line for an in-progress transfer.
///
/// `offset` — bytes transferred so far.
/// `size`   — total file size in bytes (may be 0 if unknown).
pub fn show_progress(offset: i64, size: i64) {
    // Initialise the clock on first call.
    PROGRESS_START.with(|c| {
        if c.get().is_none() {
            c.set(Some(Instant::now()));
        }
    });

    let elapsed = elapsed_since_start();
    let rate = if elapsed > 0.0 { offset as f64 / elapsed } else { 0.0 };

    let pct_str = if size > 0 {
        let pct = (offset as f64 / size as f64 * 100.0).min(100.0);
        format!("{:3.0}%", pct)
    } else {
        String::from("  ? %")
    };

    let rate_str = format_rate(rate);
    let eta_str = if size > 0 && rate > 0.0 {
        let remaining = ((size - offset) as f64 / rate) as u64;
        format_eta(remaining)
    } else {
        String::from("   -:--:--")
    };

    // \r moves to column 0; spaces pad over any longer previous line.
    eprint!(
        "\r{:>15} {} {:>12}/s{}",
        format_bytes(offset),
        pct_str,
        rate_str,
        eta_str,
    );
    let _ = std::io::stderr().flush();

    LAST_OFFSET.with(|c| c.set(offset));
    LAST_ELAPSED_SECS.with(|c| c.set(elapsed));
}

/// Print the final progress summary after a transfer completes.
///
/// `size` — total bytes transferred (same as file size for a full transfer).
pub fn end_progress(size: i64) {
    let elapsed = elapsed_since_start();
    let rate = if elapsed > 0.0 { size as f64 / elapsed } else { 0.0 };
    let rate_str = format_rate(rate);
    let elapsed_str = format_elapsed(elapsed);

    // Overwrite the live line, then move to a new line.
    eprintln!(
        "\r{:>15} 100%  {:>12}/s    {}",
        format_bytes(size),
        rate_str,
        elapsed_str,
    );

    // Reset state for the next file.
    PROGRESS_START.with(|c| c.set(None));
    LAST_OFFSET.with(|c| c.set(0));
    LAST_ELAPSED_SECS.with(|c| c.set(0.0));
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn format_bytes(n: i64) -> String {
    if n < 1_024 {
        format!("{} B", n)
    } else if n < 1_024 * 1_024 {
        format!("{:.2} KB", n as f64 / 1_024.0)
    } else if n < 1_024 * 1_024 * 1_024 {
        format!("{:.2} MB", n as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.2} GB", n as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

/// Format a bytes/second rate using K/M/G suffixes.
fn format_rate(bps: f64) -> String {
    if bps < 1_024.0 {
        format!("{:.2}  B", bps)
    } else if bps < 1_024.0 * 1_024.0 {
        format!("{:.2} KB", bps / 1_024.0)
    } else if bps < 1_024.0 * 1_024.0 * 1_024.0 {
        format!("{:.2} MB", bps / (1_024.0 * 1_024.0))
    } else {
        format!("{:.2} GB", bps / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

/// Format remaining seconds as `h:mm:ss`.
fn format_eta(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("  {:1}:{:02}:{:02}", h, m, s)
}

/// Format total elapsed time.
fn format_elapsed(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{:1}:{:02}:{:02}", h, m, s)
}
