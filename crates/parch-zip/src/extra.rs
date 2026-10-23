//! Extra fields: ZIP64, extended timestamp (UT), Info-ZIP Unix (ux), NTFS, Unicode Path.

use parch_core::entry::Time;

use crate::spec::{from_filetime, put16, put32, put64, to_filetime, Le};

pub const ZIP64: u16 = 0x0001;
pub const NTFS: u16 = 0x000a;
pub const EXT_TIME: u16 = 0x5455;
pub const UNIX_OLD: u16 = 0x5855;
pub const UNIX_NEW: u16 = 0x7875;
pub const UNICODE_PATH: u16 = 0x7075;
pub const AES: u16 = 0x9901;

/// Iterates over `(id, data)` pairs; stops at the first malformed field.
pub fn fields(data: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut r = Le::new(data);
    std::iter::from_fn(move || {
        let id = r.u16()?;
        let len = r.u16()? as usize;
        let body = r.bytes(len)?;
        Some((id, body))
    })
}

/// Copies all fields except `skip` (used to rebuild ZIP64 fields).
pub fn without(data: &[u8], skip: u16, out: &mut Vec<u8>) {
    for (id, body) in fields(data) {
        if id != skip && body.len() <= u16::MAX as usize {
            put16(out, id);
            put16(out, body.len() as u16);
            out.extend_from_slice(body);
        }
    }
}

pub fn write_zip64(out: &mut Vec<u8>, values: &[u64]) {
    if values.is_empty() {
        return;
    }
    put16(out, ZIP64);
    put16(out, (values.len() * 8) as u16);
    for &v in values {
        put64(out, v);
    }
}

fn unix32(t: Option<Time>) -> Option<i32> {
    t.and_then(|t| i32::try_from(t.secs).ok())
}

/// Extended timestamp. The local version carries mtime and atime, the central one only mtime;
/// both carry the same flags (they describe the local field).
pub fn write_ext_time(out: &mut Vec<u8>, mtime: Option<Time>, atime: Option<Time>, local: bool) {
    let (m, a) = (unix32(mtime), unix32(atime));
    let flags = m.map_or(0, |_| 1) | a.map_or(0, |_| 2);
    if flags == 0 {
        return;
    }
    let mut body = vec![flags];
    if let Some(m) = m {
        body.extend_from_slice(&m.to_le_bytes());
    }
    if local {
        if let Some(a) = a {
            body.extend_from_slice(&a.to_le_bytes());
        }
    }
    put16(out, EXT_TIME);
    put16(out, body.len() as u16);
    out.extend_from_slice(&body);
}

pub fn write_unix(out: &mut Vec<u8>, uid: u32, gid: u32) {
    put16(out, UNIX_NEW);
    put16(out, 11);
    out.push(1);
    out.push(4);
    put32(out, uid);
    out.push(4);
    put32(out, gid);
}

pub fn write_ntfs(out: &mut Vec<u8>, mtime: Time, atime: Option<Time>, btime: Option<Time>) {
    put16(out, NTFS);
    put16(out, 32);
    put32(out, 0);
    put16(out, 1);
    put16(out, 24);
    put64(out, to_filetime(mtime));
    put64(out, to_filetime(atime.unwrap_or(mtime)));
    put64(out, to_filetime(btime.unwrap_or(mtime)));
}

pub fn write_unicode_path(out: &mut Vec<u8>, raw_name: &[u8], utf8: &str) {
    put16(out, UNICODE_PATH);
    put16(out, (5 + utf8.len()) as u16);
    out.push(1);
    put32(out, crc32fast::hash(raw_name));
    out.extend_from_slice(utf8.as_bytes());
}

/// Metadata found in extra fields.
#[derive(Default, Debug)]
pub struct Parsed {
    pub mtime: Option<Time>,
    pub atime: Option<Time>,
    pub btime: Option<Time>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub unicode_path: Option<(u32, String)>,
    pub aes_method: Option<u16>,
    pub has_zip64: bool,
}

pub fn parse(data: &[u8]) -> Parsed {
    let mut p = Parsed::default();
    let mut ntfs = false;
    for (id, body) in fields(data) {
        let mut r = Le::new(body);
        match id {
            ZIP64 => p.has_zip64 = true,
            NTFS => {
                r.u32();
                while r.remaining() >= 4 {
                    let (Some(tag), Some(len)) = (r.u16(), r.u16()) else { break };
                    let Some(b) = r.bytes(len as usize) else { break };
                    if tag == 1 && len >= 24 {
                        let mut t = Le::new(b);
                        let (m, a, c) = (t.u64(), t.u64(), t.u64());
                        p.mtime = m.map(from_filetime);
                        p.atime = a.map(from_filetime);
                        p.btime = c.map(from_filetime);
                        ntfs = true;
                    }
                }
            }
            EXT_TIME if !ntfs => {
                let Some(flags) = r.u8() else { continue };
                if flags & 1 != 0 {
                    if let Some(m) = r.u32() {
                        p.mtime = Some(Time::new(m as i32 as i64, 0));
                    }
                }
                if flags & 2 != 0 {
                    if let Some(a) = r.u32() {
                        p.atime = Some(Time::new(a as i32 as i64, 0));
                    }
                }
                if flags & 4 != 0 {
                    if let Some(c) = r.u32() {
                        p.btime = Some(Time::new(c as i32 as i64, 0));
                    }
                }
            }
            UNIX_OLD => {
                if p.mtime.is_none() {
                    if let (Some(a), Some(m)) = (r.u32(), r.u32()) {
                        p.atime = Some(Time::new(a as i32 as i64, 0));
                        p.mtime = Some(Time::new(m as i32 as i64, 0));
                    }
                }
                if let (Some(u), Some(g)) = (r.u16(), r.u16()) {
                    p.uid.get_or_insert(u as u32);
                    p.gid.get_or_insert(g as u32);
                }
            }
            UNIX_NEW => {
                if r.u8() != Some(1) {
                    continue;
                }
                let uid = r.u8().and_then(|n| r.uint(n as usize));
                let gid = r.u8().and_then(|n| r.uint(n as usize));
                if let (Some(u), Some(g)) = (uid, gid) {
                    p.uid = u32::try_from(u).ok();
                    p.gid = u32::try_from(g).ok();
                }
            }
            UNICODE_PATH => {
                if r.u8() == Some(1) {
                    if let Some(crc) = r.u32() {
                        let rest = r.bytes(r.remaining()).unwrap_or_default();
                        if let Ok(s) = std::str::from_utf8(rest) {
                            p.unicode_path = Some((crc, s.to_string()));
                        }
                    }
                }
            }
            AES => {
                r.u16();
                r.u16();
                r.u8();
                p.aes_method = r.u16();
            }
            _ => {}
        }
    }
    p
}

/// Reads the ZIP64 values that replace saturated header fields, in APPNOTE order.
pub fn zip64_values(data: &[u8], usize: &mut u64, csize: &mut u64, offset: &mut u64, need: [bool; 3]) -> bool {
    let Some((_, body)) = fields(data).find(|(id, _)| *id == ZIP64) else { return !need.iter().any(|&n| n) };
    let mut r = Le::new(body);
    for (i, slot) in [usize, csize, offset].into_iter().enumerate() {
        if need[i] {
            match r.u64() {
                Some(v) => *slot = v,
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_parse_fields() {
        let mut v = Vec::new();
        write_ext_time(&mut v, Some(Time::new(1_700_000_000, 0)), Some(Time::new(1_700_000_100, 0)), true);
        write_unix(&mut v, 501, 20);
        write_unicode_path(&mut v, b"raw", "имя");
        let p = parse(&v);
        assert_eq!(p.mtime, Some(Time::new(1_700_000_000, 0)));
        assert_eq!(p.atime, Some(Time::new(1_700_000_100, 0)));
        assert_eq!((p.uid, p.gid), (Some(501), Some(20)));
        assert_eq!(p.unicode_path, Some((crc32fast::hash(b"raw"), "имя".to_string())));
    }

    #[test]
    fn ntfs_wins_over_ext_time() {
        let mut v = Vec::new();
        write_ntfs(&mut v, Time::new(1_000, 500), None, Some(Time::new(10, 0)));
        write_ext_time(&mut v, Some(Time::new(5, 0)), None, false);
        let p = parse(&v);
        assert_eq!(p.mtime, Some(Time::new(1_000, 500)));
        assert_eq!(p.btime, Some(Time::new(10, 0)));
    }

    #[test]
    fn zip64_in_order_and_rebuild() {
        let mut v = Vec::new();
        write_unix(&mut v, 1, 2);
        write_zip64(&mut v, &[10, 20]);
        let (mut u, mut c, mut o) = (0, 0, 7);
        assert!(zip64_values(&v, &mut u, &mut c, &mut o, [true, true, false]));
        assert_eq!((u, c, o), (10, 20, 7));
        let mut rest = Vec::new();
        without(&v, ZIP64, &mut rest);
        assert!(fields(&rest).all(|(id, _)| id != ZIP64));
        assert_eq!(fields(&rest).count(), 1);
    }

    #[test]
    fn malformed_fields_do_not_panic() {
        assert_eq!(fields(&[1, 0, 200, 0, 1]).count(), 0);
        let _ = parse(&[0x55, 0x54, 1, 0, 1]);
        let _ = parse(&[0x75, 0x78, 3, 0, 1, 9, 0]);
    }
}
