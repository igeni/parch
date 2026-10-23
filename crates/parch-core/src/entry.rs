use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A point in time relative to the Unix epoch (may be negative).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Time {
    pub secs: i64,
    pub nanos: u32,
}

impl Time {
    pub const fn new(secs: i64, nanos: u32) -> Self {
        Self { secs, nanos }
    }

    pub fn from_system(t: SystemTime) -> Self {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Self::new(d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => {
                let d = e.duration();
                let mut secs = -(d.as_secs() as i64);
                let mut nanos = d.subsec_nanos();
                if nanos > 0 {
                    secs -= 1;
                    nanos = 1_000_000_000 - nanos;
                }
                Self::new(secs, nanos)
            }
        }
    }

    pub fn to_system(self) -> Option<SystemTime> {
        let base = if self.secs >= 0 {
            UNIX_EPOCH.checked_add(Duration::from_secs(self.secs as u64))?
        } else {
            UNIX_EPOCH.checked_sub(Duration::from_secs(self.secs.unsigned_abs()))?
        };
        base.checked_add(Duration::from_nanos(self.nanos as u64))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// Metadata of an entry being added to an archive.
#[derive(Clone, Debug)]
pub struct EntryMeta {
    /// Archive name: `/`-separated, relative, without a trailing `/` (raw bytes on Unix).
    pub name: Vec<u8>,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix `st_mode` including the file type bits.
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Windows/DOS file attributes.
    pub dos_attrs: Option<u32>,
    pub mtime: Option<Time>,
    pub atime: Option<Time>,
    /// Creation (birth) time.
    pub btime: Option<Time>,
    pub link_target: Option<Vec<u8>>,
}

impl EntryMeta {
    pub fn new(name: Vec<u8>, kind: EntryKind) -> Self {
        Self {
            name,
            kind,
            size: 0,
            mode: None,
            uid: None,
            gid: None,
            dos_attrs: None,
            mtime: None,
            atime: None,
            btime: None,
            link_target: None,
        }
    }

    pub fn name_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }
}

pub const S_IFMT: u32 = 0o170000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_roundtrip_including_pre_epoch() {
        for t in [Time::new(0, 0), Time::new(1_700_000_000, 123), Time::new(-1, 500_000_000), Time::new(-86_400 * 365, 0)] {
            assert_eq!(Time::from_system(t.to_system().unwrap()), t);
        }
    }
}
