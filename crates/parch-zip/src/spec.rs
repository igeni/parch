//! ZIP structures (PKWARE APPNOTE 6.3.x): signatures, flags, little-endian helpers, DOS time.

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use parch_core::entry::Time;

pub const LFH_SIG: u32 = 0x0403_4b50;
pub const CDH_SIG: u32 = 0x0201_4b50;
pub const EOCD_SIG: u32 = 0x0605_4b50;
pub const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
pub const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
pub const DD_SIG: u32 = 0x0807_4b50;

pub const LFH_LEN: usize = 30;
pub const CDH_LEN: usize = 46;
pub const EOCD_LEN: usize = 22;
pub const ZIP64_EOCD_LEN: usize = 56;
pub const ZIP64_LOCATOR_LEN: usize = 20;

pub const FLAG_ENCRYPTED: u16 = 0x0001;
pub const FLAG_DESCRIPTOR: u16 = 0x0008;
pub const FLAG_UTF8: u16 = 0x0800;

pub const HOST_DOS: u8 = 0;
pub const HOST_UNIX: u8 = 3;
pub const HOST_NTFS: u8 = 10;
pub const HOST_VFAT: u8 = 14;
pub const HOST_OSX: u8 = 19;
/// "Version made by" low byte: APPNOTE 6.3.
pub const SPEC_VERSION: u8 = 63;
pub const VERSION_ZIP64: u16 = 45;

pub const U16_MAX: u64 = 0xFFFF;
pub const U32_MAX: u64 = 0xFFFF_FFFF;

pub fn is_unix_host(host: u8) -> bool {
    host == HOST_UNIX || host == HOST_OSX
}

pub fn is_dos_host(host: u8) -> bool {
    matches!(host, HOST_DOS | HOST_NTFS | HOST_VFAT | 6 | 11)
}

pub fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

pub fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

pub fn put64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// Clamps a 64-bit value into a 32-bit header field (0xFFFFFFFF means "see ZIP64").
pub fn clamp32(x: u64) -> u32 {
    x.min(U32_MAX) as u32
}

/// Bounds-checked little-endian reader.
pub struct Le<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Le<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let b = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(b)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.bytes(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Option<u64> {
        self.bytes(8).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    /// Unsigned little-endian integer of 1..=8 bytes.
    pub fn uint(&mut self, n: usize) -> Option<u64> {
        if n == 0 || n > 8 {
            return None;
        }
        let b = self.bytes(n)?;
        Some(b.iter().rev().fold(0u64, |acc, &x| (acc << 8) | x as u64))
    }
}

const DOS_MIN: (u16, u16) = (0, (1 << 5) | 1);
const DOS_MAX: (u16, u16) = ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31);

/// Converts to MS-DOS `(time, date)`: local time (or UTC), 2-second resolution rounded up,
/// clamped to 1980-01-01..2107-12-31.
pub fn to_dos(t: Option<Time>, utc: bool) -> (u16, u16) {
    let Some(t) = t else { return DOS_MIN };
    let secs = t.secs + (t.secs & 1);
    let ndt: Option<NaiveDateTime> = if utc {
        DateTime::from_timestamp(secs, 0).map(|d| d.naive_utc())
    } else {
        Local.timestamp_opt(secs, 0).earliest().map(|d| d.naive_local())
    };
    let Some(d) = ndt else { return DOS_MIN };
    if d.year() < 1980 {
        return DOS_MIN;
    }
    if d.year() > 2107 {
        return DOS_MAX;
    }
    let date = (((d.year() - 1980) as u16) << 9) | ((d.month() as u16) << 5) | d.day() as u16;
    let time = ((d.hour() as u16) << 11) | ((d.minute() as u16) << 5) | (d.second() as u16 / 2);
    (time, date)
}

/// Interprets MS-DOS `(time, date)` as local time.
pub fn from_dos(time: u16, date: u16) -> Option<Time> {
    let d = NaiveDate::from_ymd_opt(1980 + (date >> 9) as i32, ((date >> 5) & 0x0f) as u32, (date & 0x1f) as u32)?;
    let ndt = d.and_hms_opt((time >> 11) as u32, ((time >> 5) & 0x3f) as u32, ((time & 0x1f) * 2) as u32)?;
    let local = Local.from_local_datetime(&ndt).earliest()?;
    Some(Time::new(local.timestamp(), 0))
}

/// Windows FILETIME (100 ns ticks since 1601-01-01 UTC).
pub fn to_filetime(t: Time) -> u64 {
    const EPOCH_DIFF: i128 = 11_644_473_600;
    let ticks = (t.secs as i128 + EPOCH_DIFF) * 10_000_000 + (t.nanos / 100) as i128;
    ticks.clamp(0, u64::MAX as i128) as u64
}

pub fn from_filetime(ft: u64) -> Time {
    const EPOCH_DIFF: i64 = 11_644_473_600;
    let secs = (ft / 10_000_000) as i64 - EPOCH_DIFF;
    let nanos = ((ft % 10_000_000) * 100) as u32;
    Time::new(secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_time_utc_roundtrip_and_clamping() {
        let t = Time::new(1_700_000_000, 0); // 2023-11-14 22:13:20 UTC
        let (time, date) = to_dos(Some(t), true);
        assert_eq!(date >> 9, 43);
        assert_eq!((date >> 5) & 0x0f, 11);
        assert_eq!(date & 0x1f, 14);
        assert_eq!(time >> 11, 22);
        assert_eq!((time >> 5) & 0x3f, 13);
        assert_eq!((time & 0x1f) * 2, 20);
        assert_eq!(to_dos(Some(Time::new(0, 0)), true), DOS_MIN);
        assert_eq!(to_dos(Some(Time::new(5_000_000_000, 0)), true), DOS_MAX);
        assert_eq!(to_dos(None, false), DOS_MIN);
        let odd = to_dos(Some(Time::new(1_700_000_001, 0)), true);
        assert_eq!((odd.0 & 0x1f) * 2, 22);
    }

    #[test]
    fn dos_time_local_roundtrip() {
        let t = Time::new(1_600_000_000, 0);
        let (time, date) = to_dos(Some(t), false);
        assert_eq!(from_dos(time, date), Some(t));
        assert_eq!(from_dos(0, 0), None);
    }

    #[test]
    fn filetime_roundtrip() {
        for t in [Time::new(0, 0), Time::new(1_700_000_000, 123_456_700), Time::new(-1_000_000, 0)] {
            assert_eq!(from_filetime(to_filetime(t)), t);
        }
    }

    #[test]
    fn le_reader_bounds() {
        let mut r = Le::new(&[1, 0, 2, 0, 0, 0]);
        assert_eq!(r.u16(), Some(1));
        assert_eq!(r.u32(), Some(2));
        assert_eq!(r.u8(), None);
        assert_eq!(Le::new(&[0x34, 0x12]).uint(2), Some(0x1234));
    }
}
