//! Logging infrastructure mirroring log.c from rsync.
//!
//! Thin wrapper around the `log` crate with rsync-style verbosity levels
//! and a role string for the current process (sender / receiver / etc.).
//!
//! ## Debug tracing
//!
//! Two mechanisms are supported (both are zero-cost in release builds without
//! the feature flag):
//!
//! * **`RSYNC_RS_DEBUG` env var** (always available): set to any non-empty value
//!   to get `eprintln!`-style traces on stderr.
//! * **`debug-trace` feature** (opt-in at compile time): replaces `rdebug!`
//!   output with structured `tracing::debug!` events.  Enable with:
//!   ```
//!   cargo build --features debug-trace
//!   RUST_LOG=rsync_rs=debug ./rsync-rs ...
//!   ```

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use log::{error, warn, info, debug};

// ── Global state ──────────────────────────────────────────────────────────────

/// Global verbosity level (0 = quiet, 1 = normal, 2+ = verbose).
static VERBOSITY: AtomicI32 = AtomicI32::new(0);

/// Cached `RSYNC_RS_DEBUG` env-var presence.  Read once, used everywhere.
static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);
static DEBUG_INIT: std::sync::Once = std::sync::Once::new();

/// Returns true when verbose internal tracing is enabled (RSYNC_RS_DEBUG set).
#[inline]
pub fn is_debug() -> bool {
    DEBUG_INIT.call_once(|| {
        DEBUG_ENABLED.store(std::env::var_os("RSYNC_RS_DEBUG").is_some(), Ordering::Relaxed);
    });
    DEBUG_ENABLED.load(Ordering::Relaxed)
}

/// Conditional debug trace macro.
///
/// When the `debug-trace` feature is enabled, emits a `tracing::debug!` event
/// (controlled by `RUST_LOG`).  Otherwise falls back to an `eprintln!` guarded
/// by the `RSYNC_RS_DEBUG` environment variable.
#[macro_export]
macro_rules! rdebug {
    ($($arg:tt)*) => {
        #[cfg(feature = "debug-trace")]
        {
            tracing::debug!($($arg)*);
        }
        #[cfg(not(feature = "debug-trace"))]
        {
            if $crate::log_mod::is_debug() {
                eprintln!($($arg)*);
            }
        }
    };
}

/// Role string — set once by main before any fork.
static mut WHO: &str = "rsync";

// ── Log level ─────────────────────────────────────────────────────────────────

/// rsync-style log levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Warning = 1,
    Info = 2,
    Debug = 3,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warning => "WARNING",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
        }
    }
}

// ── Init / verbosity ──────────────────────────────────────────────────────────

/// Initialise the underlying `env_logger`.
/// Call once at programme start (before `set_verbosity`).
pub fn log_init() {
    let _ = env_logger::try_init();
}

/// Set the global verbosity level.
pub fn set_verbosity(v: i32) {
    VERBOSITY.store(v, Ordering::Relaxed);
}

/// Read the global verbosity level.
pub fn get_verbosity() -> i32 {
    VERBOSITY.load(Ordering::Relaxed)
}

// ── Role string ───────────────────────────────────────────────────────────────

/// Set the role string for the current process (e.g. "sender", "receiver").
/// # Safety
/// Must be called before any threads are spawned.
pub unsafe fn set_who(who: &'static str) {
    WHO = who;
}

/// Return the role string for the current process.
pub fn who_am_i() -> &'static str {
    // SAFETY: WHO is only mutated before threads are spawned.
    unsafe { WHO }
}

// ── rprintf macro ────────────────────────────────────────────────────────────

/// Write a formatted message at the given `LogLevel`.
///
/// - `Error` / `Warning` → `eprintln!` (stderr) and the `log` crate.
/// - `Info` / `Debug`    → `println!`  (stdout) and the `log` crate.
///
/// Respects the global verbosity level: `Info` requires verbosity ≥ 1,
/// `Debug` requires verbosity ≥ 2.
#[macro_export]
macro_rules! rprintf {
    ($level:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {{
        use $crate::log_mod::{LogLevel, get_verbosity};
        let lvl: LogLevel = $level;
        let verbose = get_verbosity();
        let should_print = match lvl {
            LogLevel::Error | LogLevel::Warning => true,
            LogLevel::Info  => verbose >= 1,
            LogLevel::Debug => verbose >= 2,
        };
        if should_print {
            let msg = format!($fmt $(, $arg)*);
            match lvl {
                LogLevel::Error => {
                    eprintln!("[{}] ERROR: {}", $crate::log_mod::who_am_i(), msg);
                    log::error!("{}", msg);
                }
                LogLevel::Warning => {
                    eprintln!("[{}] WARNING: {}", $crate::log_mod::who_am_i(), msg);
                    log::warn!("{}", msg);
                }
                LogLevel::Info => {
                    println!("{}", msg);
                    log::info!("{}", msg);
                }
                LogLevel::Debug => {
                    println!("[{}] {}", $crate::log_mod::who_am_i(), msg);
                    log::debug!("{}", msg);
                }
            }
        }
    }};
}

// ── Convenience wrappers ──────────────────────────────────────────────────────

/// Log at `Error` level (always printed).
pub fn rlog_error(msg: &str) {
    eprintln!("[{}] ERROR: {}", who_am_i(), msg);
    error!("{}", msg);
}

/// Log at `Warning` level (always printed).
pub fn rlog_warn(msg: &str) {
    eprintln!("[{}] WARNING: {}", who_am_i(), msg);
    warn!("{}", msg);
}

/// Log at `Info` level (requires verbosity ≥ 1).
pub fn rlog_info(msg: &str) {
    if get_verbosity() >= 1 {
        println!("{}", msg);
        info!("{}", msg);
    }
}

/// Log at `Debug` level (requires verbosity ≥ 2).
pub fn rlog_debug(msg: &str) {
    if get_verbosity() >= 2 {
        println!("[{}] {}", who_am_i(), msg);
        debug!("{}", msg);
    }
}
