//! UID / GID list management mirroring rsync's `uidlist.c`.
//!
//! Maintains sender-side uid→name / gid→name mappings and the wire protocol
//! for transmitting them (protocol ≥ 30).

#![allow(dead_code)]

use std::collections::HashMap;
use anyhow::{Context, Result};
use crate::io::varint::{read_varint, write_varint, read_byte, write_byte};

// ── UidList ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UidList {
    /// uid → username (populated on sender or after recv).
    pub uid_map: HashMap<u32, String>,
    /// gid → group name.
    pub gid_map: HashMap<u32, String>,
}

impl Default for UidList {
    fn default() -> Self {
        Self::new()
    }
}

impl UidList {
    pub fn new() -> Self {
        UidList {
            uid_map: HashMap::new(),
            gid_map: HashMap::new(),
        }
    }

    // ── sender-side population ────────────────────────────────────────────

    /// Look up the username for `uid` and add it to `uid_map` if not already
    /// present.  On Unix, uses `nix::unistd::User`; on other platforms the
    /// numeric string is used as a fallback.
    pub fn add_uid(&mut self, uid: u32) {
        if self.uid_map.contains_key(&uid) {
            return;
        }
        let name = Self::uid_to_name(uid);
        self.uid_map.insert(uid, name);
    }

    /// Look up the group name for `gid` and add it to `gid_map` if not already
    /// present.
    pub fn add_gid(&mut self, gid: u32) {
        if self.gid_map.contains_key(&gid) {
            return;
        }
        let name = Self::gid_to_name(gid);
        self.gid_map.insert(gid, name);
    }

    // ── wire protocol — send ──────────────────────────────────────────────

    /// Encode and write the uid→name list.
    ///
    /// Wire format: `varint(id), byte(name_len), bytes(name)` … `varint(0)`.
    pub fn send_uid_list<W: std::io::Write>(&self, w: &mut W) -> Result<()> {
        Self::send_map(w, &self.uid_map)
    }

    /// Encode and write the gid→name list.
    pub fn send_gid_list<W: std::io::Write>(&self, w: &mut W) -> Result<()> {
        Self::send_map(w, &self.gid_map)
    }

    // ── wire protocol — receive ───────────────────────────────────────────

    /// Read a uid→name list from the wire, merging into `uid_map`.
    pub fn recv_uid_list<R: std::io::Read>(&mut self, r: &mut R) -> Result<()> {
        Self::recv_map(r, &mut self.uid_map)
    }

    /// Read a gid→name list from the wire, merging into `gid_map`.
    pub fn recv_gid_list<R: std::io::Read>(&mut self, r: &mut R) -> Result<()> {
        Self::recv_map(r, &mut self.gid_map)
    }

    // ── mapping ───────────────────────────────────────────────────────────

    /// Map a received (remote) uid to a local uid.
    ///
    /// Looks up the name stored in `uid_map` for `uid`, then resolves that
    /// name to the corresponding local uid.  Returns `0` if the uid is not
    /// in the map or the name has no local counterpart.
    pub fn map_uid(&self, uid: u32) -> u32 {
        if let Some(name) = self.uid_map.get(&uid) {
            if let Some(local) = Self::name_to_uid(name) {
                return local;
            }
        }
        0
    }

    /// Map a received (remote) gid to a local gid.  Returns `0` on failure.
    pub fn map_gid(&self, gid: u32) -> u32 {
        if let Some(name) = self.gid_map.get(&gid) {
            if let Some(local) = Self::name_to_gid(name) {
                return local;
            }
        }
        0
    }

    // ── private helpers ───────────────────────────────────────────────────

    fn send_map<W: std::io::Write>(w: &mut W, map: &HashMap<u32, String>) -> Result<()> {
        // Sort for deterministic output.
        let mut entries: Vec<(&u32, &String)> = map.iter().collect();
        entries.sort_by_key(|(id, _)| *id);

        for (id, name) in entries {
            // Skip id=0: the terminator value.
            if *id == 0 {
                continue;
            }
            let name_bytes = name.as_bytes();
            // Clamp name length to 255 bytes as the wire uses a single byte length.
            let len = name_bytes.len().min(255) as u8;
            write_varint(w, *id as i32).context("send_map: write id")?;
            write_byte(w, len).context("send_map: write name_len")?;
            if len > 0 {
                w.write_all(&name_bytes[..len as usize])
                    .context("send_map: write name")?;
            }
        }
        // End-of-list marker.
        write_varint(w, 0).context("send_map: write terminator")
    }

    fn recv_map<R: std::io::Read>(r: &mut R, map: &mut HashMap<u32, String>) -> Result<()> {
        loop {
            let id = read_varint(r).context("recv_map: read id")?;
            if id == 0 {
                break;
            }
            let len = read_byte(r).context("recv_map: read name_len")? as usize;
            let name = if len > 0 {
                let mut buf = vec![0u8; len];
                r.read_exact(&mut buf).context("recv_map: read name")?;
                String::from_utf8_lossy(&buf).into_owned()
            } else {
                String::new()
            };
            map.insert(id as u32, name);
        }
        Ok(())
    }

    // ── platform-specific name lookups ────────────────────────────────────

    #[cfg(unix)]
    fn uid_to_name(uid: u32) -> String {
        use nix::unistd::{User, Uid};
        User::from_uid(Uid::from_raw(uid))
            .ok()
            .flatten()
            .map(|u| u.name)
            .unwrap_or_else(|| uid.to_string())
    }

    #[cfg(not(unix))]
    fn uid_to_name(uid: u32) -> String {
        uid.to_string()
    }

    #[cfg(unix)]
    fn gid_to_name(gid: u32) -> String {
        use nix::unistd::{Group, Gid};
        Group::from_gid(Gid::from_raw(gid))
            .ok()
            .flatten()
            .map(|g| g.name)
            .unwrap_or_else(|| gid.to_string())
    }

    #[cfg(not(unix))]
    fn gid_to_name(gid: u32) -> String {
        gid.to_string()
    }

    #[cfg(unix)]
    fn name_to_uid(name: &str) -> Option<u32> {
        use nix::unistd::User;
        User::from_name(name)
            .ok()
            .flatten()
            .map(|u| u.uid.as_raw())
    }

    #[cfg(not(unix))]
    fn name_to_uid(_name: &str) -> Option<u32> {
        None
    }

    #[cfg(unix)]
    fn name_to_gid(name: &str) -> Option<u32> {
        use nix::unistd::Group;
        Group::from_name(name)
            .ok()
            .flatten()
            .map(|g| g.gid.as_raw())
    }

    #[cfg(not(unix))]
    fn name_to_gid(_name: &str) -> Option<u32> {
        None
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip_uid(uid_map: HashMap<u32, String>) -> HashMap<u32, String> {
        let mut list = UidList::new();
        list.uid_map = uid_map;

        let mut buf = Vec::new();
        list.send_uid_list(&mut buf).unwrap();

        let mut received = UidList::new();
        received.recv_uid_list(&mut Cursor::new(&buf)).unwrap();
        received.uid_map
    }

    #[test]
    fn send_recv_uid_list_roundtrip() {
        let mut map = HashMap::new();
        map.insert(1000u32, "alice".to_string());
        map.insert(1001u32, "bob".to_string());

        let got = round_trip_uid(map.clone());
        assert_eq!(got.get(&1000), Some(&"alice".to_string()));
        assert_eq!(got.get(&1001), Some(&"bob".to_string()));
    }

    #[test]
    fn send_recv_gid_list_roundtrip() {
        let mut list = UidList::new();
        list.gid_map.insert(100u32, "users".to_string());
        list.gid_map.insert(0u32, "root".to_string()); // id=0 is skipped on send

        let mut buf = Vec::new();
        list.send_gid_list(&mut buf).unwrap();

        let mut received = UidList::new();
        received.recv_gid_list(&mut Cursor::new(&buf)).unwrap();

        assert_eq!(received.gid_map.get(&100), Some(&"users".to_string()));
        // id=0 is suppressed by the sender (used as end-of-list marker)
        assert!(!received.gid_map.contains_key(&0));
    }

    #[test]
    fn empty_list_terminates_immediately() {
        let list = UidList::new();
        let mut buf = Vec::new();
        list.send_uid_list(&mut buf).unwrap();
        // Only the varint(0) terminator should be written.
        assert_eq!(buf, vec![0x00]);
    }

    #[test]
    fn name_truncated_at_255() {
        let long_name = "x".repeat(300);
        let mut list = UidList::new();
        list.uid_map.insert(42, long_name);

        let mut buf = Vec::new();
        list.send_uid_list(&mut buf).unwrap();

        let mut received = UidList::new();
        received.recv_uid_list(&mut Cursor::new(&buf)).unwrap();

        let name = received.uid_map.get(&42).unwrap();
        assert_eq!(name.len(), 255);
    }

    #[test]
    fn add_uid_dedup() {
        let mut list = UidList::new();
        list.add_uid(0);
        list.add_uid(0);
        assert_eq!(list.uid_map.len(), 1);
    }

    #[test]
    fn map_uid_unknown_returns_zero() {
        let list = UidList::new();
        assert_eq!(list.map_uid(9999), 0);
    }

    #[test]
    fn map_gid_unknown_returns_zero() {
        let list = UidList::new();
        assert_eq!(list.map_gid(9999), 0);
    }

    #[test]
    fn multiple_entries_sorted_deterministically() {
        let mut list = UidList::new();
        list.uid_map.insert(5, "e".to_string());
        list.uid_map.insert(3, "c".to_string());
        list.uid_map.insert(1, "a".to_string());

        let mut buf = Vec::new();
        list.send_uid_list(&mut buf).unwrap();

        let mut received = UidList::new();
        received.recv_uid_list(&mut Cursor::new(&buf)).unwrap();

        assert_eq!(received.uid_map.get(&1), Some(&"a".to_string()));
        assert_eq!(received.uid_map.get(&3), Some(&"c".to_string()));
        assert_eq!(received.uid_map.get(&5), Some(&"e".to_string()));
    }
}
