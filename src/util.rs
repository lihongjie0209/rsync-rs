//! Utility functions mirroring util1.c / util2.c from rsync.

#![allow(dead_code)]

use crate::protocol::constants::{BLOCK_SIZE, MAX_BLOCK_SIZE};
use std::time::Duration;

// ── Block-size helpers ────────────────────────────────────────────────────────

/// Choose a block length for rolling-checksum computation.
///
/// Mirrors the logic in `sender.c` / `generator.c`:
/// ```text
/// blength = MAX(BLOCK_SIZE, file_size / 10000);
/// if (blength > MAX_BLOCK_SIZE) blength = MAX_BLOCK_SIZE;
/// ```
pub fn block_len_for_file(file_size: i64) -> i32 {
    let blength = std::cmp::max(BLOCK_SIZE as i64, file_size / 10_000);
    std::cmp::min(blength, MAX_BLOCK_SIZE as i64) as i32
}

/// Number of blocks for `file_size` bytes at `blength` bytes/block.
pub fn sum_count_for_file(file_size: i64, blength: i32) -> i32 {
    if blength <= 0 || file_size <= 0 {
        return 0;
    }
    ((file_size + blength as i64 - 1) / blength as i64) as i32
}

/// Length of the last (possibly partial) block.
/// Returns 0 when `file_size` divides `blength` exactly (last == full block).
pub fn remainder_for_file(file_size: i64, blength: i32) -> i32 {
    if blength <= 0 || file_size <= 0 {
        return 0;
    }
    (file_size % blength as i64) as i32
}

// ── Human-readable sizes ──────────────────────────────────────────────────────

/// Format a number with comma thousands-separators, matching C rsync's
/// `do_big_num(num, 0, NULL)` / `big_num()` default output.
///
/// Examples: 0 → "0", 1234567 → "1,234,567", -1234 → "-1,234"
pub fn big_num(n: i64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let negative = n < 0;
    // Work with magnitude, handling i64::MIN carefully
    let mut remaining = if negative {
        // avoid overflow: handle first digit in negated form
        n.unsigned_abs()
    } else {
        n as u64
    };

    let mut digits: Vec<u8> = Vec::with_capacity(26);
    while remaining > 0 {
        digits.push((remaining % 10) as u8);
        remaining /= 10;
    }
    // digits is now least-significant first; insert commas every 3
    let mut result = String::with_capacity(digits.len() + digits.len() / 3 + 2);
    for (i, d) in digits.iter().rev().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push((b'0' + d) as char);
    }
    if negative {
        format!("-{}", result)
    } else {
        result
    }
}

/// Format a byte count matching rsync's `human_num()`.
///
/// Behaviour depends on `human_readable` level (mirrors the C global):
/// - 0 (default): comma-separated decimal, e.g. `1,234,567`
/// - 1 (`-h`):    same as 0 but with 1000-based K/M/G when ≥ 1000
/// - 2+ (`-hh`):  1024-based K/M/G with two decimal places
///
/// In practice rsync's stats output uses level 0 by default, so the
/// numbers look like `"sent 1,234 bytes"`.
pub fn human_num_level(n: i64, level: u8) -> String {
    if level >= 2 {
        // 1024-based units with 2 decimal places  
        let mult = 1024u64;
        let abs = n.unsigned_abs();
        if abs < mult {
            return big_num(n);
        }
        let (val, unit) = scale_units(abs, mult);
        let signed = if n < 0 { -val } else { val };
        return format!("{:.2}{}", signed, unit);
    }
    if level == 1 {
        // 1000-based units
        let mult = 1000u64;
        let abs = n.unsigned_abs();
        if abs < mult {
            return big_num(n);
        }
        let (val, unit) = scale_units(abs, mult);
        let signed = if n < 0 { -val } else { val };
        return format!("{:.2}{}", signed, unit);
    }
    // level 0: comma-separated decimal (default)
    big_num(n)
}

fn scale_units(abs: u64, mult: u64) -> (f64, char) {
    const UNITS: &[char] = &['K', 'M', 'G', 'T', 'P'];
    let mut val = abs as f64 / mult as f64;
    let mut idx = 0;
    while val >= mult as f64 && idx + 1 < UNITS.len() {
        val /= mult as f64;
        idx += 1;
    }
    (val, UNITS[idx])
}

/// Default human_num (level 0 = comma-separated, matching C rsync default).
pub fn human_num(n: i64) -> String {
    human_num_level(n, 0)
}

/// Format a float as comma-separated decimal with given decimal places.
/// Matches rsync's `comma_dnum()`.
pub fn comma_dnum(val: f64, decimal_digits: usize) -> String {
    let formatted = format!("{:.prec$}", val, prec = decimal_digits);
    // Split on '.' and comma-format the integer part
    if let Some(dot_pos) = formatted.find('.') {
        let (int_part, frac_part) = formatted.split_at(dot_pos);
        let int_val: i64 = int_part.parse().unwrap_or(0);
        format!("{}{}", big_num(int_val), frac_part)
    } else {
        let int_val: i64 = formatted.parse().unwrap_or(0);
        big_num(int_val)
    }
}

// ── File identity ─────────────────────────────────────────────────────────────

/// Return `true` when two `Metadata` objects refer to the same inode.
///
/// On Unix this compares `(dev, ino)`.  On Windows all files are considered
/// distinct (no stable inode concept in std metadata).
#[cfg(unix)]
pub fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
pub fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    false
}

// ── Sleep ────────────────────────────────────────────────────────────────────

/// Sleep for `ms` milliseconds.
pub fn ms_sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// ── itemize formatter ────────────────────────────────────────────────────────

/// Port of `log.c::log_formatted`'s `'i'` case (the `--itemize-changes`
/// 11-character indicator).  Format: `YXcstpoguax`.
///
/// * `Y` — update kind: `<`/`>` transfer, `c` create, `h` hardlink, `.` no
///   change, `*` deleted (handled with the `*deleting` short-form below).
/// * `X` — file kind: `f`/`d`/`L`/`D`/`S`.
/// * remaining 9 chars are flags or `.` when not set.
///
/// `mode` is the file's POSIX mode (used to decide `X`).
/// `am_sender` toggles `<` vs `>` for transfers.
pub fn iflags_to_str(iflags: u32, mode: u32, am_sender: bool) -> String {
    use crate::protocol::constants::*;
    if iflags & ITEM_DELETED != 0 {
        return "*deleting  ".to_string();
    }
    let mut c = ['.'; 11];

    c[0] = if iflags & ITEM_LOCAL_CHANGE != 0 {
        if iflags & ITEM_XNAME_FOLLOWS != 0 { 'h' } else { 'c' }
    } else if iflags & ITEM_TRANSFER == 0 {
        '.'
    } else if am_sender {
        '<'
    } else {
        '>'
    };

    let ftype = mode & 0o170000;
    let is_link = ftype == 0o120000;
    let is_dir = ftype == 0o040000;
    let is_blk = ftype == 0o060000;
    let is_chr = ftype == 0o020000;
    let is_fifo = ftype == 0o010000;
    let is_sock = ftype == 0o140000;
    c[1] = if is_link {
        'L'
    } else if is_dir {
        'd'
    } else if is_fifo || is_sock {
        'S'
    } else if is_blk || is_chr {
        'D'
    } else {
        'f'
    };

    if is_link {
        c[3] = '.';
        c[4] = if iflags & ITEM_REPORT_TIME == 0 { '.' } else { 't' };
    } else {
        c[3] = if iflags & ITEM_REPORT_SIZE == 0 { '.' } else { 's' };
        c[4] = if iflags & ITEM_REPORT_TIME == 0 { '.' } else { 't' };
    }
    c[2] = if iflags & ITEM_REPORT_CHANGE == 0 { '.' } else { 'c' };
    c[5] = if iflags & ITEM_REPORT_PERMS == 0 { '.' } else { 'p' };
    c[6] = if iflags & ITEM_REPORT_OWNER == 0 { '.' } else { 'o' };
    c[7] = if iflags & ITEM_REPORT_GROUP == 0 { '.' } else { 'g' };
    let atime_or_crtime = ITEM_REPORT_ATIME | ITEM_REPORT_CRTIME;
    c[8] = if iflags & atime_or_crtime == 0 {
        '.'
    } else if iflags & atime_or_crtime == atime_or_crtime {
        'b'
    } else if iflags & ITEM_REPORT_ATIME != 0 {
        'u'
    } else {
        'n'
    };
    c[9] = if iflags & ITEM_REPORT_ACL == 0 { '.' } else { 'a' };
    c[10] = if iflags & ITEM_REPORT_XATTR == 0 { '.' } else { 'x' };

    if iflags & (ITEM_IS_NEW | ITEM_MISSING_DATA) != 0 {
        let ch = if iflags & ITEM_IS_NEW != 0 { '+' } else { '?' };
        for i in 2..11 {
            c[i] = ch;
        }
    } else if c[0] == '.' || c[0] == 'h' || c[0] == 'c' {
        // If nothing changed past c[1], collapse the trailing slots to spaces.
        if c[2..].iter().all(|&x| x == '.') {
            for i in 2..11 {
                c[i] = ' ';
            }
        }
    }
    c.iter().collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_len_min() {
        // Small files → BLOCK_SIZE
        assert_eq!(block_len_for_file(0), BLOCK_SIZE as i32);
        assert_eq!(block_len_for_file(100), BLOCK_SIZE as i32);
        assert_eq!(block_len_for_file(7_000_000), BLOCK_SIZE as i32); // 7M/10000=700
    }

    #[test]
    fn block_len_scales() {
        // 100 MB → 10 000 bytes
        assert_eq!(block_len_for_file(100_000_000), 10_000);
    }

    #[test]
    fn block_len_capped() {
        // Very large file → MAX_BLOCK_SIZE
        assert_eq!(block_len_for_file(i64::MAX), MAX_BLOCK_SIZE);
    }

    #[test]
    fn sum_count_and_remainder() {
        let size = 1_000i64;
        let blen = 700i32;
        assert_eq!(sum_count_for_file(size, blen), 2);
        assert_eq!(remainder_for_file(size, blen), 300);
    }

    #[test]
    fn human_num_small() {
        assert_eq!(human_num(0), "0");
        assert_eq!(human_num(999), "999");
        assert_eq!(human_num(1000), "1,000");
        assert_eq!(human_num(1023), "1,023");
    }

    #[test]
    fn big_num_commas() {
        assert_eq!(big_num(0), "0");
        assert_eq!(big_num(1234), "1,234");
        assert_eq!(big_num(1234567), "1,234,567");
        assert_eq!(big_num(-1234), "-1,234");
        assert_eq!(big_num(100), "100");
        assert_eq!(big_num(1000000), "1,000,000");
    }

    #[test]
    fn human_num_with_units() {
        // level 2 (1024-based)
        assert_eq!(human_num_level(1024, 2), "1.00K");
        assert_eq!(human_num_level(1024 * 1024, 2), "1.00M");
    }

    #[test]
    fn comma_dnum_format() {
        assert_eq!(comma_dnum(1234.5, 2), "1,234.50");
        assert_eq!(comma_dnum(0.5, 2), "0.50");
    }

    #[test]
    fn iflags_new_regular_file() {
        use crate::protocol::constants::*;
        let s = iflags_to_str(ITEM_IS_NEW | ITEM_TRANSFER, 0o100644, false);
        assert_eq!(s, ">f+++++++++");
    }

    #[test]
    fn iflags_updated_size_time() {
        use crate::protocol::constants::*;
        let s = iflags_to_str(
            ITEM_TRANSFER | ITEM_REPORT_SIZE | ITEM_REPORT_TIME,
            0o100644, false);
        assert_eq!(s, ">f.st......");
    }

    #[test]
    fn iflags_deleted_short_form() {
        use crate::protocol::constants::*;
        let s = iflags_to_str(ITEM_DELETED, 0o100644, false);
        assert_eq!(s, "*deleting  ");
    }

    #[test]
    fn iflags_no_change_collapses_to_spaces() {
        // No transfer, no flags → trailing dots collapse to spaces.
        let s = iflags_to_str(0, 0o100644, false);
        assert_eq!(s, ".f         ");
    }

    #[test]
    fn iflags_sender_uses_left_arrow() {
        use crate::protocol::constants::*;
        let s = iflags_to_str(ITEM_IS_NEW | ITEM_TRANSFER, 0o100644, true);
        assert_eq!(s, "<f+++++++++");
    }

    #[test]
    fn iflags_directory_kind() {
        use crate::protocol::constants::*;
        let s = iflags_to_str(ITEM_IS_NEW | ITEM_TRANSFER, 0o040755, false);
        assert_eq!(s, ">d+++++++++");
    }

    #[test]
    fn iflags_symlink_kind_and_no_size_slot() {
        use crate::protocol::constants::*;
        // Symlinks have c[3]='.' regardless of REPORT_SIZE.
        let s = iflags_to_str(
            ITEM_TRANSFER | ITEM_REPORT_SIZE | ITEM_REPORT_TIME,
            0o120777, false);
        assert_eq!(s, ">L..t......");
    }
}
