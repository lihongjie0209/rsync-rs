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
    CF_INC_RECURSE, CF_VARINT_FLIST_FLAGS, XMIT_EXTENDED_FLAGS, XMIT_GROUP_NAME_FOLLOWS,
    XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_MOD_NSEC,
    XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_TIME, XMIT_SAME_UID,
    XMIT_USER_NAME_FOLLOWS, FLAG_HLINKED,
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
    recv_file_list_ex(r, protocol, checksum_len, true, true, CF_VARINT_FLIST_FLAGS)
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
    compat_flags: u32,
) -> anyhow::Result<FileList> {
    let varint_flags = (compat_flags & CF_VARINT_FLIST_FLAGS) != 0;
    let inc_recurse = (compat_flags & CF_INC_RECURSE) != 0;
    let mut flist = FileList::new();
    let mut prev: Option<FileInfo> = None;

    loop {
        let xflags = if varint_flags {
            read_varint(r)? as u32
        } else {
            // flist.c:2624-2640 — byte/shortint xfer flags.
            let mut flags = read_byte(r)? as u32;
            if flags == 0 {
                0
            } else {
                if protocol >= 28 && (flags & XMIT_EXTENDED_FLAGS) != 0 {
                    flags |= (read_byte(r)? as u32) << 8;
                }
                if flags == (XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST) {
                    // End-of-flist with inline io_error.
                    let _io_error = read_varint(r)?;
                    0
                } else {
                    flags
                }
            }
        };

        if xflags == 0 {
            // End-of-list. With xfer_flags_as_varint, the io_error follows
            // inline (already-consumed for non-varint above when ENDLIST bit).
            if varint_flags {
                let _io_error = read_varint(r)?;
            }
            // Trailing id-name lists. Skipped when inc_recurse is on.
            if !inc_recurse && protocol >= 30 {
                if preserve_uid {
                    loop {
                        let id = read_varint30(r)? as u32;
                        if id == 0 {
                            break;
                        }
                        let len = read_byte(r)? as usize;
                        let mut name = vec![0u8; len];
                        if len > 0 {
                            r.read_exact(&mut name)?;
                        }
                    }
                }
                if preserve_gid {
                    loop {
                        let id = read_varint30(r)? as u32;
                        if id == 0 {
                            break;
                        }
                        let len = read_byte(r)? as usize;
                        let mut name = vec![0u8; len];
                        if len > 0 {
                            r.read_exact(&mut name)?;
                        }
                    }
                }
            } else if protocol < 30 {
                let _io_error = read_int(r)?;
            }
            break;
        }
        let fi = recv_file_entry_inner(r, xflags, prev.as_ref(), protocol, checksum_len, &flist.files, flist.ndx_start)?;
        prev = Some(fi.clone());
        flist.files.push(fi);
    }

    // Sort the received flist so our NDX order matches C rsync's receiver.
    // C's receiver also calls flist_sort_and_clean after receiving entries.
    // However, C rsync assigns hardlink first_hlink_ndx values based on the
    // SENDER's sorted order which may differ from ours.  After sorting, we
    // remap hard_link_first_ndx so it reflects the new (post-sort) positions.
    let n = flist.files.len();

    // Sort using (original_index, file) so we can build the permutation.
    let mut indexed: Vec<(usize, FileInfo)> = flist.files.drain(..).enumerate().collect();
    indexed.sort_by(|(_, a), (_, b)| crate::flist::sort::file_compare(a, b));

    // Build old_ndx → new_pos mapping.
    let mut old_to_new = vec![0usize; n];
    for (new_pos, (old_pos, _)) in indexed.iter().enumerate() {
        old_to_new[*old_pos] = new_pos;
    }
    flist.files = indexed.into_iter().map(|(_, fi)| fi).collect();

    // Remap hardlink leader references to reflect the new positions.
    for fi in flist.files.iter_mut() {
        if fi.hard_link_first_ndx >= 0 {
            let old_ndx = (fi.hard_link_first_ndx - flist.ndx_start) as usize;
            if old_ndx < old_to_new.len() {
                fi.hard_link_first_ndx = (old_to_new[old_ndx] as i32) + flist.ndx_start;
            }
        }
    }

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
    recv_file_entry_inner(r, xflags, prev, protocol, checksum_len, &[], 0)
}

/// Core decoder: reconstruct a [`FileInfo`] given already-read `xflags`.
///
/// `flist_so_far` and `ndx_start` are used to resolve hardlink first-member
/// references for protocol 30+ non-first members.
fn recv_file_entry_inner<R: Read>(
    r: &mut R,
    xflags: u32,
    prev: Option<&FileInfo>,
    protocol: u32,
    checksum_len: usize,
    flist_so_far: &[FileInfo],
    ndx_start: i32,
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

    // ── hardlink non-first member (protocol 30+) ──────────────────────────
    // Wire format: xflags (XMIT_HLINKED set, XMIT_HLINK_FIRST NOT set) + name
    // + first_hlink_ndx (varint).  All remaining metadata is omitted; we copy
    // it from the first entry in this flist batch.
    if protocol >= 30
        && (xflags & XMIT_HLINKED != 0)
        && (xflags & XMIT_HLINK_FIRST == 0)
    {
        let first_hlink_ndx = read_varint(r)? as i32;
        let rel_idx = first_hlink_ndx - ndx_start;
        if rel_idx >= 0 && (rel_idx as usize) < flist_so_far.len() {
            let first = &flist_so_far[rel_idx as usize];
            return Ok(FileInfo {
                name: basename,
                dirname,
                modtime: first.modtime,
                mod_nsec: first.mod_nsec,
                size: first.size,
                mode: first.mode,
                flags: FLAG_HLINKED,
                uid: first.uid,
                gid: first.gid,
                link_target: first.link_target.clone(),
                rdev_major: first.rdev_major,
                rdev_minor: first.rdev_minor,
                hard_link_first_ndx: first_hlink_ndx,
                dev: 0,
                ino: 0,
                checksum: None,
            });
        }
        // Out-of-range reference: the sender is referencing an entry from a
        // prior batch (cross-batch hardlink).  We cannot resolve it without
        // storing the prior batch, so report an error.
        return Err(anyhow::anyhow!(
            "hardlink reference {first_hlink_ndx} out of range \
             (ndx_start={ndx_start}, entries_so_far={})",
            flist_so_far.len()
        ));
    }

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
        let id = read_varint(r)? as u32;
        if xflags & XMIT_USER_NAME_FOLLOWS != 0 {
            // Inline user name follows: 1-byte length + name bytes.
            let len = read_byte(r)? as usize;
            let mut name = vec![0u8; len];
            if len > 0 {
                r.read_exact(&mut name)?;
            }
        }
        id
    } else {
        read_int(r)? as u32
    };

    let gid = if xflags & XMIT_SAME_GID != 0 {
        prev.map(|p| p.gid).unwrap_or(0)
    } else if protocol >= 30 {
        let id = read_varint(r)? as u32;
        if xflags & XMIT_GROUP_NAME_FOLLOWS != 0 {
            let len = read_byte(r)? as usize;
            let mut name = vec![0u8; len];
            if len > 0 {
                r.read_exact(&mut name)?;
            }
        }
        id
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
        dev: 0,
        ino: 0,
        checksum,
    })
}
