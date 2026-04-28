//! Sending the file list over the wire.
//!
//! Implements the *sender* side of rsync's flist protocol (protocol 30/31).
//! Flags are encoded as varints (`xfer_flags_as_varint` path) for simplicity.

use std::io::Write;

use crate::io::varint::{
    write_byte, write_int, write_shortint, write_varlong, write_varint, write_varint30,
};
use crate::protocol::constants::{
    CF_INC_RECURSE, CF_VARINT_FLIST_FLAGS, XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST,
    XMIT_LONG_NAME, XMIT_MOD_NSEC, XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_TIME,
    XMIT_SAME_UID,
};
use crate::protocol::types::{FileInfo, FileList, FileType};

/// Transmit the complete file list.
///
/// Files are walked in sorted order (`flist.sorted`).  After the last entry an
/// end-of-list marker (`write_varint(0)`) followed by the I/O-error code is
/// written.
#[derive(Clone, Copy, Debug, Default)]
pub struct Preserve {
    pub uid: bool,
    pub gid: bool,
    pub times: bool,
    pub devices: bool,
}

pub fn send_file_list<W: Write>(
    w: &mut W,
    flist: &FileList,
    protocol: u32,
    checksum_len: usize,
    io_error: i32,
) -> anyhow::Result<()> {
    send_file_list_ex(
        w,
        flist,
        protocol,
        checksum_len,
        io_error,
        Preserve { uid: true, gid: true, times: true, devices: true },
        CF_VARINT_FLIST_FLAGS,
    )
}

pub fn send_file_list_ex<W: Write>(
    w: &mut W,
    flist: &FileList,
    protocol: u32,
    checksum_len: usize,
    io_error: i32,
    preserve: Preserve,
    compat_flags: u32,
) -> anyhow::Result<()> {
    let varint_flags = (compat_flags & CF_VARINT_FLIST_FLAGS) != 0;
    let inc_recurse = (compat_flags & CF_INC_RECURSE) != 0;
    let use_safe_inc_flist = protocol >= 31;

    let order: Vec<usize> = if flist.sorted.is_empty() {
        (0..flist.files.len()).collect()
    } else {
        flist.sorted.clone()
    };

    let mut prev: Option<&FileInfo> = None;
    for &idx in &order {
        let fi = &flist.files[idx];
        send_file_entry(w, fi, prev, protocol, checksum_len, preserve, varint_flags)?;
        prev = Some(fi);
    }

    // End-of-list trailer (flist.c::write_end_of_flist:2077-2087).
    crate::rdebug!(
        "[flist-send] writing end-of-list (varint_flags={}, inc_recurse={}, use_safe_inc_flist={})",
        varint_flags,
        inc_recurse,
        use_safe_inc_flist
    );
    if varint_flags {
        write_varint(w, 0)?;
        write_varint(w, io_error)?;
    } else if use_safe_inc_flist || io_error != 0 {
        // Send the io_error inline via the EXTENDED|IO_ERROR_ENDLIST shortint.
        write_shortint(w, (XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST) as u16)?;
        write_varint(w, io_error)?;
    } else {
        write_byte(w, 0)?;
    }

    // Trailing uid/gid name lists (flist.c:2513). With inc_recurse=1 the
    // sender does NOT append id-lists at the end of the initial flist; names
    // are sent inline per-entry via XMIT_USER_NAME_FOLLOWS instead.
    if !inc_recurse && protocol >= 30 {
        if preserve.uid {
            write_varint30(w, 0)?;
        }
        if preserve.gid {
            write_varint30(w, 0)?;
        }
    } else if protocol < 30 {
        write_int(w, io_error)?;
    }

    Ok(())
}

/// Encode a single file entry onto the wire.
///
/// `prev` is the last entry that was sent (used to compute delta fields).
fn send_file_entry<W: Write>(
    w: &mut W,
    fi: &FileInfo,
    prev: Option<&FileInfo>,
    protocol: u32,
    checksum_len: usize,
    preserve: Preserve,
    varint_flags: bool,
) -> anyhow::Result<()> {
    let fname = fi.path();
    let prev_name = prev.map(|p| p.path()).unwrap_or_default();

    // ── name prefix sharing ───────────────────────────────────────────────
    // Count how many leading bytes are shared with the previous entry name,
    // up to a maximum of 255 (the on-wire byte limit).
    let same_len: usize = fname
        .bytes()
        .zip(prev_name.bytes())
        .take(255)
        .take_while(|(a, b)| a == b)
        .count();
    let rest = &fname[same_len..];
    let rest_len = rest.len();

    // ── build xflags ──────────────────────────────────────────────────────
    let mut xflags: u32 = 0;

    if same_len > 0 {
        xflags |= XMIT_SAME_NAME;
    }
    if rest_len > 255 {
        xflags |= XMIT_LONG_NAME;
    }

    if let Some(p) = prev {
        if fi.mode == p.mode {
            xflags |= XMIT_SAME_MODE;
        }
        if preserve.times && fi.modtime == p.modtime {
            xflags |= XMIT_SAME_TIME;
        } else if !preserve.times {
            xflags |= XMIT_SAME_TIME;
        }
        if preserve.uid && fi.uid == p.uid {
            xflags |= XMIT_SAME_UID;
        } else if !preserve.uid {
            xflags |= XMIT_SAME_UID;
        }
        if preserve.gid && fi.gid == p.gid {
            xflags |= XMIT_SAME_GID;
        } else if !preserve.gid {
            xflags |= XMIT_SAME_GID;
        }
    } else {
        // First file: no XMIT_SAME_* unless preserve-flag is off (then suppress field).
        if !preserve.times {
            xflags |= XMIT_SAME_TIME;
        }
        if !preserve.uid {
            xflags |= XMIT_SAME_UID;
        }
        if !preserve.gid {
            xflags |= XMIT_SAME_GID;
        }
    }
    if fi.mod_nsec != 0 && protocol >= 31 {
        xflags |= XMIT_MOD_NSEC;
    }

    // A zero flags value would be misread as the end-of-list marker.
    if xflags == 0 {
        xflags = XMIT_EXTENDED_FLAGS;
    }

    // ── write flags ───────────────────────────────────────────────────────
    if varint_flags {
        write_varint(w, xflags as i32)?;
    } else {
        // flist.c:551-558 — byte/shortint encoding gated by high bits.
        if (xflags & 0xFF00) != 0 {
            xflags |= XMIT_EXTENDED_FLAGS;
            write_shortint(w, xflags as u16)?;
        } else {
            write_byte(w, (xflags & 0xFF) as u8)?;
        }
    }

    // ── name ─────────────────────────────────────────────────────────────
    if xflags & XMIT_SAME_NAME != 0 {
        write_byte(w, same_len as u8)?;
    }
    if xflags & XMIT_LONG_NAME != 0 {
        write_varint30(w, rest_len as i32)?;
    } else {
        write_byte(w, rest_len as u8)?;
    }
    w.write_all(rest.as_bytes())?;

    // ── file length ───────────────────────────────────────────────────────
    write_varlong(w, fi.size, 3)?;

    // ── modtime ───────────────────────────────────────────────────────────
    if xflags & XMIT_SAME_TIME == 0 {
        if protocol >= 30 {
            write_varlong(w, fi.modtime, 4)?;
        } else {
            write_int(w, fi.modtime as i32)?;
        }
    }
    if xflags & XMIT_MOD_NSEC != 0 {
        write_varint(w, fi.mod_nsec as i32)?;
    }

    // ── mode ─────────────────────────────────────────────────────────────
    if xflags & XMIT_SAME_MODE == 0 {
        write_int(w, fi.mode as i32)?;
    }

    // ── uid / gid ─────────────────────────────────────────────────────────
    if preserve.uid && xflags & XMIT_SAME_UID == 0 {
        if protocol >= 30 {
            write_varint(w, fi.uid as i32)?;
        } else {
            write_int(w, fi.uid as i32)?;
        }
    }
    if preserve.gid && xflags & XMIT_SAME_GID == 0 {
        if protocol >= 30 {
            write_varint(w, fi.gid as i32)?;
        } else {
            write_int(w, fi.gid as i32)?;
        }
    }

    // ── device rdev (protocol >= 28, always send both major and minor) ────
    let ft = fi.file_type();
    let send_rdev = preserve.devices
        && (matches!(ft, FileType::Device)
            || (matches!(ft, FileType::Special) && protocol < 31));
    if send_rdev {
        write_varint30(w, fi.rdev_major as i32)?;
        if protocol >= 30 {
            write_varint(w, fi.rdev_minor as i32)?;
        } else {
            write_int(w, fi.rdev_minor as i32)?;
        }
    }

    // ── symlink target ────────────────────────────────────────────────────
    if matches!(ft, FileType::Symlink) {
        let target = fi.link_target.as_deref().unwrap_or("");
        write_varint30(w, target.len() as i32)?;
        w.write_all(target.as_bytes())?;
    }

    // ── always-checksum (regular files only) ─────────────────────────────
    if checksum_len > 0 && fi.is_regular() {
        match fi.checksum.as_deref() {
            Some(sum) => {
                let n = checksum_len.min(sum.len());
                w.write_all(&sum[..n])?;
                if n < checksum_len {
                    w.write_all(&vec![0u8; checksum_len - n])?;
                }
            }
            None => {
                w.write_all(&vec![0u8; checksum_len])?;
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::flist::recv::recv_file_list;
    use crate::flist::sort::flist_sort;
    use crate::protocol::types::FileList;

    fn reg_file(path: &str, size: i64, modtime: i64) -> FileInfo {
        let (dirname, name) = if let Some(p) = path.rfind('/') {
            (Some(path[..p].to_string()), path[p + 1..].to_string())
        } else {
            (None, path.to_string())
        };
        FileInfo {
            name,
            dirname,
            size,
            modtime,
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn roundtrip_single_file() {
        let mut flist = FileList::new();
        flist.files.push(reg_file("hello.txt", 42, 1700000000));
        flist_sort(&mut flist);

        let mut buf = Vec::<u8>::new();
        send_file_list(&mut buf, &flist, 31, 0, 0).unwrap();

        let got = recv_file_list(&mut buf.as_slice(), 31, 0).unwrap();
        assert_eq!(got.files.len(), 1);
        assert_eq!(got.files[0].path(), "hello.txt");
        assert_eq!(got.files[0].size, 42);
        assert_eq!(got.files[0].modtime, 1700000000);
        assert_eq!(got.files[0].mode, 0o100644);
        assert_eq!(got.files[0].uid, 1000);
        assert_eq!(got.files[0].gid, 1000);
    }

    #[test]
    fn roundtrip_multiple_files() {
        let mut flist = FileList::new();
        flist.files.push(reg_file("a/b/c.txt", 100, 1000));
        flist.files.push(reg_file("a/b/d.txt", 200, 1001));
        flist.files.push(reg_file("a/e.txt", 300, 999));
        flist_sort(&mut flist);

        let mut buf = Vec::<u8>::new();
        send_file_list(&mut buf, &flist, 31, 0, 0).unwrap();

        let got = recv_file_list(&mut buf.as_slice(), 31, 0).unwrap();
        assert_eq!(got.files.len(), 3);

        // Collect paths in sorted send order
        let sent_paths: Vec<String> = flist.sorted.iter()
            .map(|&i| flist.files[i].path())
            .collect();
        let recv_paths: Vec<String> = got.files.iter().map(|f| f.path()).collect();
        assert_eq!(sent_paths, recv_paths);
    }

    #[test]
    fn roundtrip_symlink() {
        let mut flist = FileList::new();
        let mut fi = reg_file("link", 0, 1700000000);
        fi.mode = 0o120777;
        fi.link_target = Some("target".to_string());
        flist.files.push(fi);
        flist_sort(&mut flist);

        let mut buf = Vec::<u8>::new();
        send_file_list(&mut buf, &flist, 31, 0, 0).unwrap();

        let got = recv_file_list(&mut buf.as_slice(), 31, 0).unwrap();
        assert_eq!(got.files[0].link_target.as_deref(), Some("target"));
    }

    #[test]
    fn roundtrip_with_checksum() {
        let mut flist = FileList::new();
        let mut fi = reg_file("file.bin", 16, 1700000000);
        fi.checksum = Some(vec![0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        flist.files.push(fi);
        flist_sort(&mut flist);

        let checksum_len = 16;
        let mut buf = Vec::<u8>::new();
        send_file_list(&mut buf, &flist, 31, checksum_len, 0).unwrap();

        let got = recv_file_list(&mut buf.as_slice(), 31, checksum_len).unwrap();
        assert_eq!(
            got.files[0].checksum.as_deref(),
            Some(&[0xDE, 0xAD, 0xBE, 0xEF, 0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..])
        );
    }

    #[test]
    fn roundtrip_mod_nsec() {
        let mut flist = FileList::new();
        let mut fi = reg_file("nsec.txt", 0, 1700000000);
        fi.mod_nsec = 123_456_789;
        flist.files.push(fi);
        flist_sort(&mut flist);

        let mut buf = Vec::<u8>::new();
        send_file_list(&mut buf, &flist, 31, 0, 0).unwrap();

        let got = recv_file_list(&mut buf.as_slice(), 31, 0).unwrap();
        assert_eq!(got.files[0].mod_nsec, 123_456_789);
    }
}
