//! Filesystem scanning. Directories are read by pool workers in parallel; each directory is a job.

use std::fs::{self, FileType, Metadata};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::entry::{EntryKind, EntryMeta};
use crate::filter::Filter;
use crate::fsmeta;
use crate::report::Reporter;

#[derive(Default)]
pub struct ScanOpts {
    pub follow_symlinks: bool,
    pub dir_entries: bool,
    pub filter: Filter,
    /// Files never added (the archive being written); compared after canonicalization.
    pub skip: Vec<PathBuf>,
}

impl ScanOpts {
    fn skipped(&self, path: &Path) -> bool {
        if self.skip.is_empty() {
            return false;
        }
        let Some(name) = path.file_name() else { return false };
        if !self.skip.iter().any(|s| s.file_name() == Some(name)) {
            return false;
        }
        fs::canonicalize(path).is_ok_and(|c| self.skip.contains(&c))
    }
}

/// An entry found on disk.
#[derive(Clone, Debug)]
pub struct Found {
    pub meta: EntryMeta,
    pub path: PathBuf,
}

/// A directory waiting to be read.
pub struct DirJob {
    pub path: PathBuf,
    pub name: Vec<u8>,
    /// Canonical paths of ancestors (only tracked when following symlinks).
    pub ancestors: Arc<Vec<PathBuf>>,
}

pub trait ScanSink {
    fn found(&mut self, f: Found);
    fn dir(&mut self, d: DirJob);
}

/// Converts a user-supplied path into an archive name, dropping root, prefix and `..` components.
/// Returns the name and whether anything was dropped.
pub fn archive_name(path: &Path) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut stripped = false;
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => stripped = true,
            Component::CurDir => {}
            Component::Normal(s) => {
                if !out.is_empty() {
                    out.push(b'/');
                }
                out.extend_from_slice(&fsmeta::os_bytes(s));
            }
        }
    }
    (out, stripped)
}

fn child_name(parent: &[u8], file_name: &std::ffi::OsStr) -> Vec<u8> {
    let part = fsmeta::os_bytes(file_name);
    let mut name = Vec::with_capacity(parent.len() + 1 + part.len());
    name.extend_from_slice(parent);
    if !name.is_empty() {
        name.push(b'/');
    }
    name.extend_from_slice(&part);
    name
}

/// Handles a command-line input path.
pub fn scan_root(input: &Path, base: Option<&Path>, opts: &ScanOpts, sink: &mut dyn ScanSink, report: &Reporter) {
    let path = match base {
        Some(b) => b.join(input),
        None => input.to_path_buf(),
    };
    let (name, stripped) = archive_name(input);
    if stripped {
        report.warn(format!("{}: removing leading '/', drive or '..' from archive names", input.display()));
    }
    let md = if opts.follow_symlinks { fs::metadata(&path) } else { fs::symlink_metadata(&path) };
    let md = match md {
        Ok(m) => m,
        Err(e) => {
            report.warn(format!("{}: {e}", path.display()));
            return;
        }
    };
    if name.is_empty() {
        if md.is_dir() {
            let ancestors = Arc::new(canonical_ancestor(opts, &path, &[]).unwrap_or_default());
            sink.dir(DirJob { path, name, ancestors });
        } else {
            report.warn(format!("{}: cannot derive an archive name", input.display()));
        }
        return;
    }
    handle(path, name, md.file_type(), Some(md), &Arc::new(Vec::new()), opts, sink, report);
}

/// Reads one directory, emitting entries and subdirectory jobs.
pub fn scan_dir(job: &DirJob, opts: &ScanOpts, sink: &mut dyn ScanSink, report: &Reporter) {
    let rd = match fs::read_dir(&job.path) {
        Ok(r) => r,
        Err(e) => {
            report.warn(format!("{}: {e}", job.path.display()));
            return;
        }
    };
    for ent in rd {
        let ent = match ent {
            Ok(e) => e,
            Err(e) => {
                report.warn(format!("{}: {e}", job.path.display()));
                continue;
            }
        };
        let name = child_name(&job.name, &ent.file_name());
        let ft = match ent.file_type() {
            Ok(t) => t,
            Err(e) => {
                report.warn(format!("{}: {e}", ent.path().display()));
                continue;
            }
        };
        // DirEntry::metadata is free on Windows and an lstat on Unix.
        let md = if ft.is_symlink() && opts.follow_symlinks { None } else { ent.metadata().ok() };
        handle(ent.path(), name, ft, md, &job.ancestors, opts, sink, report);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle(
    path: PathBuf,
    name: Vec<u8>,
    ft: FileType,
    md: Option<Metadata>,
    ancestors: &Arc<Vec<PathBuf>>,
    opts: &ScanOpts,
    sink: &mut dyn ScanSink,
    report: &Reporter,
) {
    let name_str = String::from_utf8_lossy(&name).into_owned();
    if opts.filter.excluded(&name_str) {
        return;
    }
    let md = match md {
        Some(m) => m,
        None => {
            let r = if opts.follow_symlinks { fs::metadata(&path) } else { fs::symlink_metadata(&path) };
            match r {
                Ok(m) => m,
                Err(e) => {
                    report.warn(format!("{}: {e}", path.display()));
                    return;
                }
            }
        }
    };
    let ft = if opts.follow_symlinks { md.file_type() } else { ft };
    if ft.is_dir() {
        let ancestors = if opts.follow_symlinks {
            match canonical_ancestor(opts, &path, ancestors) {
                Some(a) => Arc::new(a),
                None => {
                    report.warn(format!("{}: symbolic link loop, skipping", path.display()));
                    return;
                }
            }
        } else {
            ancestors.clone()
        };
        if opts.dir_entries && opts.filter.included(&name_str) {
            let mut meta = EntryMeta::new(name.clone(), EntryKind::Dir);
            fsmeta::fill_meta(&mut meta, &md);
            sink.found(Found { meta, path: path.clone() });
        }
        sink.dir(DirJob { path, name, ancestors });
    } else if ft.is_file() {
        if !opts.filter.included(&name_str) || opts.skipped(&path) {
            return;
        }
        let mut meta = EntryMeta::new(name, EntryKind::File);
        meta.size = md.len();
        fsmeta::fill_meta(&mut meta, &md);
        sink.found(Found { meta, path });
    } else if ft.is_symlink() {
        if !opts.filter.included(&name_str) {
            return;
        }
        let target = match fs::read_link(&path) {
            Ok(t) => t,
            Err(e) => {
                report.warn(format!("{}: {e}", path.display()));
                return;
            }
        };
        let target = link_target_bytes(target.as_os_str());
        let mut meta = EntryMeta::new(name, EntryKind::Symlink);
        fsmeta::fill_meta(&mut meta, &md);
        meta.size = target.len() as u64;
        meta.link_target = Some(target);
        sink.found(Found { meta, path });
    } else {
        report.warn(format!("{}: not a regular file, directory or symlink, skipping", path.display()));
    }
}

fn link_target_bytes(t: &std::ffi::OsStr) -> Vec<u8> {
    let b = fsmeta::os_bytes(t).into_owned();
    if cfg!(windows) { b.into_iter().map(|c| if c == b'\\' { b'/' } else { c }).collect() } else { b }
}

/// Appends the canonical form of `path` to `ancestors`; `None` on a loop.
fn canonical_ancestor(opts: &ScanOpts, path: &Path, ancestors: &[PathBuf]) -> Option<Vec<PathBuf>> {
    if !opts.follow_symlinks {
        return Some(Vec::new());
    }
    let c = fs::canonicalize(path).ok()?;
    if ancestors.contains(&c) {
        return None;
    }
    let mut v = ancestors.to_vec();
    v.push(c);
    Some(v)
}

/// Reads a list of paths (one per line, or NUL-separated).
pub fn parse_list(data: &[u8], nul: bool) -> Vec<PathBuf> {
    let sep = if nul { b'\0' } else { b'\n' };
    data.split(|&b| b == sep)
        .map(|l| if !nul && l.ends_with(b"\r") { &l[..l.len() - 1] } else { l })
        .filter(|l| !l.is_empty())
        .map(bytes_to_path)
        .collect()
}

fn bytes_to_path(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(windows)]
    {
        PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names_are_relative_and_clean() {
        assert_eq!(archive_name(Path::new("a/b/c.txt")), (b"a/b/c.txt".to_vec(), false));
        assert_eq!(archive_name(Path::new("./a/./b")), (b"a/b".to_vec(), false));
        assert_eq!(archive_name(Path::new("../x/y")), (b"x/y".to_vec(), true));
        assert_eq!(archive_name(Path::new(".")), (Vec::new(), false));
        #[cfg(unix)]
        assert_eq!(archive_name(Path::new("/abs/p")), (b"abs/p".to_vec(), true));
    }

    #[test]
    fn list_parsing() {
        assert_eq!(parse_list(b"a\nb\r\n\nc", false), vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]);
        assert_eq!(parse_list(b"a b\0c\0", true), vec![PathBuf::from("a b"), PathBuf::from("c")]);
    }
}
