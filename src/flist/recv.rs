//! Receiving the file list from the wire.
//!
//! Implements the *receiver* side of rsync's flist protocol (protocol 30/31).
//! Flags are read as varints, matching the `xfer_flags_as_varint` path used
//! by the companion [`crate::flist::send`] module.

use std::io::Read;

use crate::io::varint::{
    read_byte, read_int, read_varlong, read_varint, read_varint30,
};
use crate::protocol::constants::{
    XMIT_LONG_NAME, XMIT_MOD_NSEC, XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME,
    XMIT_SAME_TIME, XMIT_SAME_UID,
};
use crate::protocol::types::{FileInfo, FileList, FileType};

/// Read a complete file list from the wire.
///
/// Entries are read until the end-of-list marker (flags varint == 0).  The
/// I/O-error varint that follows the marker is consumed but not returned.
pub fn recv_file_list<R: Read>(
    r: &mut R,
    protocol: u32,
    checksum_len: usize,
) -> anyhow::Result<FileList> {
    recv_file_list_ex(r, protocol, checksum_len, true, true)
}

/// Same as [`recv_file_list`] but with explicit gating of the trailing
/// uid/gid name lists. Mirrors C's `recv_id_list` (uidlist.c:460), which
/// only reads each id list when `preserve_uid` / `preserve_gid` is set.
pub fn recv_file_list_ex<R: Read>(
    r: &mut R,
    protocol: u32,
    checksum_len: usize,
    preserve_uid: bool,
    preserve_gid: bool,
) -> anyhow::Result<FileList> {
    let mut flist = FileList::new();
    let mut prev: Option<FileInfo> = None;

    loop {
        let xflags = read_varint(r)? as u32;
        if xflags == 0 {
            // End-of-list. For protocol 30+ the sender now writes uid/gid
            // name lists (each terminated by varint(0)) and does NOT write
            // an inline io_error (any I/O error is sent later via
            // MSG_IO_ERROR through the mux channel). For protocol <30 a
            // 4-byte io_error int is written here instead.
            if protocol >= 30 {
                // With xfer_flags_as_varint (CF_VARINT_FLIST_FLAGS) the end
                // marker is followed by an inline io_error varint
                // (flist.c::write_end_of_flist:2079-2081), THEN the uid+gid
                // name lists. Each id list ends with varint30(0).
                let _io_error = read_varint(r)?;
                if preserve_uid {
                    // uid list
                    loop {
                        let id = read_varint30(r)? as u32;
                        if id == 0 { break; }
                        let len = read_byte(r)? as usize;
                        let mut name = vec![0u8; len];
                        if len > 0 { r.read_exact(&mut name)?; }
                    }
                }
                if preserve_gid {
                    // gid list
                    loop {
                        let id = read_varint30(r)? as u32;
                        if id == 0 { break; }
                        let len = read_byte(r)? as usize;
                        let mut name = vec![0u8; len];
                        if len > 0 { r.read_exact(&mut name)?; }
                    }
                }
            } else {
                let _io_error = read_int(r)?;
            }
            break;
        }
        let fi = recv_file_entry_inner(r, xflags, prev.as_ref(), protocol, checksum_len)?;
        prev = Some(fi.clone());
        flist.files.push(fi);
    }

    // C's send_file_list calls flist_sort_and_clean AFTER sending entries
    // (flist.c:2509). The receiver does the same so that NDX indices match
    // on both sides. We must sort here too.
    crate::flist::sort::flist_sort(&mut flist);

    flist.sorted = (0..flist.files.len()).collect();
    Ok(flist)
}

/// Read a single file entry from the wire (flags first, then the rest).
///
/// This is the public entry point when the flags have not yet been consumed.
pub fn recv_file_entry<R: Read>(
    r: &mut R,
    prev: Option<&FileInfo>,
    protocol: u32,
    checksum_len: usize,
) -> anyhow::Result<FileInfo> {
    let xflags = read_varint(r)? as u32;
    recv_file_entry_inner(r, xflags, prev, protocol, checksum_len)
}

/// Core decoder: reconstruct a [`FileInfo`] given already-read `xflags`.
fn recv_file_entry_inner<R: Read>(
    r: &mut R,
    xflags: u32,
    prev: Option<&FileInfo>,
    protocol: u32,
    checksum_len: usize,
) -> anyhow::Result<FileInfo> {
    let prev_name = prev.map(|p| p.path()).unwrap_or_default();

    // ── name ─────────────────────────────────────────────────────────────
    let same_len: usize = if xflags & XMIT_SAME_NAME != 0 {
        read_byte(r)? as usize
    } else {
        0
    };
    let rest_len: usize = if xflags & XMIT_LONG_NAME != 0 {
        read_varint30(r)? as usize
    } else {
        read_byte(r)? as usize
    };

    // Combine the shared prefix with the newly-received suffix.
    let mut name_bytes: Vec<u8> = prev_name.as_bytes()[..same_len].to_vec();
    let mut rest_buf = vec![0u8; rest_len];
    r.read_exact(&mut rest_buf)?;
    name_bytes.extend_from_slice(&rest_buf);
    let full_name = String::from_utf8_lossy(&name_bytes).into_owned();

    // Split into dirname (optional) and basename.
    let (dirname, basename) = if let Some(pos) = full_name.rfind('/') {
        (Some(full_name[..pos].to_string()), full_name[pos + 1..].to_string())
    } else {
        (None, full_name)
    };

    // ── file length ───────────────────────────────────────────────────────
    let size = read_varlong(r, 3)?;

    // ── modtime ───────────────────────────────────────────────────────────
    let modtime = if xflags & XMIT_SAME_TIME != 0 {
        prev.map(|p| p.modtime).unwrap_or(0)
    } else if protocol >= 30 {
        read_varlong(r, 4)?
    } else {
        read_int(r)? as i64
    };

    let mod_nsec = if xflags & XMIT_MOD_NSEC != 0 {
        read_varint(r)? as u32
    } else {
        0
    };

    // ── mode ─────────────────────────────────────────────────────────────
    let mode = if xflags & XMIT_SAME_MODE != 0 {
        prev.map(|p| p.mode).unwrap_or(0o100644)
    } else {
        read_int(r)? as u32
    };

    // ── uid / gid ─────────────────────────────────────────────────────────
    let uid = if xflags & XMIT_SAME_UID != 0 {
        prev.map(|p| p.uid).unwrap_or(0)
    } else if protocol >= 30 {
        read_varint(r)? as u32
    } else {
        read_int(r)? as u32
    };

    let gid = if xflags & XMIT_SAME_GID != 0 {
        prev.map(|p| p.gid).unwrap_or(0)
    } else if protocol >= 30 {
        read_varint(r)? as u32
    } else {
        read_int(r)? as u32
    };

    let ft = FileType::from_mode(mode);

    // ── device rdev ───────────────────────────────────────────────────────
    let send_rdev =
        matches!(ft, FileType::Device) || (matches!(ft, FileType::Special) && protocol < 31);
    let (rdev_major, rdev_minor) = if send_rdev {
        let major = read_varint30(r)? as u32;
        let minor = if protocol >= 30 {
            read_varint(r)? as u32
        } else {
            read_int(r)? as u32
        };
        (major, minor)
    } else {
        // Not transmitted; fall back to previous values (or 0 for first entry).
        (
            prev.map(|p| p.rdev_major).unwrap_or(0),
            prev.map(|p| p.rdev_minor).unwrap_or(0),
        )
    };

    // ── symlink target ────────────────────────────────────────────────────
    let link_target = if matches!(ft, FileType::Symlink) {
        let len = read_varint30(r)? as usize;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    } else {
        None
    };

    // ── always-checksum (regular files only) ─────────────────────────────
    let checksum = if checksum_len > 0 && matches!(ft, FileType::Regular) {
        let mut buf = vec![0u8; checksum_len];
        r.read_exact(&mut buf)?;
        Some(buf)
    } else {
        None
    };

    Ok(FileInfo {
        name: basename,
        dirname,
        modtime,
        mod_nsec,
        size,
        mode,
        flags: 0,
        uid,
        gid,
        link_target,
        rdev_major,
        rdev_minor,
        hard_link_first_ndx: -1,
        checksum,
    })
}
