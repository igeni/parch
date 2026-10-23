//! Platform-specific filesystem helpers: positional reads, metadata capture and restore, symlinks.

use std::fs::{self, File, Metadata};
use std::io;
use std::path::Path;

use crate::entry::{EntryMeta, Time};
use crate::format::EntryInfo;

/// Reads at `offset` without moving a shared cursor (Unix); safe for concurrent use of one handle.
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::FileExt::seek_read(file, buf, offset)
    }
}

pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(file, buf, offset) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of file")),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Copies timestamps, permissions and ownership from filesystem metadata.
pub fn fill_meta(meta: &mut EntryMeta, md: &Metadata) {
    meta.mtime = md.modified().ok().map(Time::from_system);
    meta.atime = md.accessed().ok().map(Time::from_system);
    meta.btime = md.created().ok().map(Time::from_system);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode = Some(md.mode());
        meta.uid = Some(md.uid());
        meta.gid = Some(md.gid());
        meta.mtime = Some(Time::new(md.mtime(), md.mtime_nsec() as u32));
        meta.atime = Some(Time::new(md.atime(), md.atime_nsec() as u32));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.dos_attrs = Some(md.file_attributes());
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RestoreOpts {
    pub perms: bool,
    pub times: bool,
    pub owner: bool,
}

const DOS_READONLY: u32 = 0x01;

fn file_times(info: &EntryInfo) -> Option<fs::FileTimes> {
    let mtime = info.mtime.and_then(Time::to_system)?;
    let mut ft = fs::FileTimes::new().set_modified(mtime);
    if let Some(t) = info.atime.and_then(Time::to_system) {
        ft = ft.set_accessed(t);
    }
    #[cfg(windows)]
    if let Some(t) = info.btime.and_then(Time::to_system) {
        use std::os::windows::fs::FileTimesExt;
        ft = ft.set_created(t);
    }
    Some(ft)
}

/// Restores metadata through an open handle of an extracted regular file.
pub fn restore_file(file: &File, info: &EntryInfo, opts: RestoreOpts) -> io::Result<()> {
    if opts.times {
        if let Some(ft) = file_times(info) {
            file.set_times(ft)?;
        }
    }
    #[cfg(unix)]
    if opts.owner {
        if let (Some(uid), Some(gid)) = (info.uid, info.gid) {
            std::os::unix::fs::fchown(file, Some(uid), Some(gid))?;
        }
    }
    if opts.perms {
        #[cfg(unix)]
        if let Some(mode) = info.mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
            return Ok(());
        }
        if info.dos_attrs.is_some_and(|a| a & DOS_READONLY != 0) {
            let mut p = file.metadata()?.permissions();
            p.set_readonly(true);
            file.set_permissions(p)?;
        }
    }
    Ok(())
}

/// Restores attributes that need a path (Windows hidden/system flags).
pub fn restore_path_attrs(path: &Path, info: &EntryInfo, opts: RestoreOpts) -> io::Result<()> {
    #[cfg(windows)]
    if opts.perms {
        if let Some(attrs) = info.dos_attrs {
            use std::os::windows::ffi::OsStrExt;
            const KEEP: u32 = 0x01 | 0x02 | 0x04 | 0x20; // readonly, hidden, system, archive
            if attrs & (0x02 | 0x04) != 0 {
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let ok = unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), attrs & KEEP) };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (path, info, opts);
    Ok(())
}

/// Restores times and permissions of an extracted directory.
pub fn restore_dir(path: &Path, info: &EntryInfo, opts: RestoreOpts) -> io::Result<()> {
    if opts.times {
        if let Some(ft) = file_times(info) {
            open_dir_for_meta(path)?.set_times(ft)?;
        }
    }
    #[cfg(unix)]
    {
        if opts.owner {
            if let (Some(uid), Some(gid)) = (info.uid, info.gid) {
                std::os::unix::fs::chown(path, Some(uid), Some(gid))?;
            }
        }
        if opts.perms {
            if let Some(mode) = info.mode {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
    }
    Ok(())
}

fn open_dir_for_meta(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        File::open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)
    }
}

/// Creates a symbolic link `link` pointing to `target` (raw bytes from the archive).
pub fn symlink(target: &[u8], link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), link)
    }
    #[cfg(windows)]
    {
        let t = String::from_utf8_lossy(target).replace('/', "\\");
        let is_dir = link.parent().is_some_and(|p| p.join(&t).is_dir());
        if is_dir {
            std::os::windows::fs::symlink_dir(&t, link)
        } else {
            std::os::windows::fs::symlink_file(&t, link)
        }
    }
}

/// Whether names in `dir` are compared case-insensitively by the filesystem.
pub fn case_insensitive(dir: &Path) -> bool {
    let id = std::process::id();
    let lower = dir.join(format!(".parch-case-probe-{id}"));
    let upper = dir.join(format!(".PARCH-CASE-PROBE-{id}"));
    if File::create(&lower).is_err() {
        return cfg!(any(windows, target_vendor = "apple"));
    }
    let folded = upper.exists();
    let _ = fs::remove_file(&lower);
    folded
}

/// Converts an `OsStr` path component to archive-name bytes.
pub fn os_bytes(s: &std::ffi::OsStr) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(s.as_bytes())
    }
    #[cfg(windows)]
    {
        match s.to_string_lossy() {
            std::borrow::Cow::Borrowed(b) => std::borrow::Cow::Borrowed(b.as_bytes()),
            std::borrow::Cow::Owned(o) => std::borrow::Cow::Owned(o.into_bytes()),
        }
    }
}
