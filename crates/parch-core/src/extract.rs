//! Parallel extraction and testing.
//!
//! Names are sanitized before anything touches the disk; target collisions are resolved up front
//! so that workers never write the same path. Directories are created first, then files in
//! parallel, then symlinks (so no file is ever written through a link from the archive), and
//! finally directory metadata from the deepest level up.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

use crate::entry::{EntryKind, Time};
use crate::filter::Filter;
use crate::format::{ArchiveReader, EntryInfo};
use crate::fsmeta::{self, RestoreOpts};
use crate::report::Reporter;
use crate::{IoResultExt, Result};

/// Entries up to this size are decoded in memory in one go.
const WHOLE_LIMIT: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overwrite {
    /// Keep existing files (warning).
    Never,
    /// Replace existing files.
    Always,
    /// Replace only files older than the archive entry.
    Newer,
}

pub struct ExtractConfig {
    pub threads: usize,
    pub dest: PathBuf,
    pub overwrite: Overwrite,
    pub strip_components: usize,
    pub flat: bool,
    pub restore: RestoreOpts,
    pub allow_unsafe_symlinks: bool,
}

/// Indices of entries accepted by `filter`.
pub fn select(reader: &dyn ArchiveReader, filter: &Filter) -> Vec<usize> {
    reader.entries().iter().enumerate().filter(|(_, e)| filter.accepts(&e.name)).map(|(i, _)| i).collect()
}

/// Splits an archive name into safe relative components.
/// Returns `None` for names that must not be extracted (`..`, NUL).
pub fn sanitize(name: &str) -> Option<(Vec<String>, bool)> {
    if name.contains('\0') {
        return None;
    }
    let mut stripped = false;
    let mut out = Vec::new();
    let name = name.replace('\\', "/");
    for (i, part) in name.split('/').enumerate() {
        match part {
            "" => {
                if i == 0 {
                    stripped = true;
                }
            }
            "." => {}
            ".." => return None,
            p if i == 0 && p.len() == 2 && p.as_bytes()[1] == b':' && p.as_bytes()[0].is_ascii_alphabetic() => stripped = true,
            p => out.push(fix_component(p)),
        }
    }
    Some((out, stripped))
}

#[cfg(windows)]
fn fix_component(p: &str) -> String {
    let mut s: String = p.chars().map(|c| if "<>:\"|?*".contains(c) || (c as u32) < 32 { '_' } else { c }).collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
        s.push('_');
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    s
}

#[cfg(not(windows))]
fn fix_component(p: &str) -> String {
    p.to_string()
}

/// Whether a symlink at `rel` (components) with `target` resolves outside the extraction root.
pub fn link_escapes(rel: &[String], target: &[u8]) -> bool {
    let t = String::from_utf8_lossy(target).replace('\\', "/");
    if t.starts_with('/') || t.as_bytes().get(1) == Some(&b':') {
        return true;
    }
    let mut depth = rel.len() as isize - 1;
    for part in t.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

struct Planned {
    index: usize,
    rel: Vec<String>,
    kind: EntryKind,
}

fn join(dest: &Path, rel: &[String]) -> PathBuf {
    let mut p = dest.to_path_buf();
    p.extend(rel);
    p
}

pub fn extract(reader: &dyn ArchiveReader, selected: &[usize], cfg: &ExtractConfig, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    fs::create_dir_all(&cfg.dest).at(&cfg.dest)?;
    let fold = fsmeta::case_insensitive(&cfg.dest);
    let key = |rel: &[String]| {
        let k = rel.join("/");
        if fold { k.to_lowercase() } else { k }
    };
    let entries = reader.entries();

    let mut planned = Vec::new();
    for &i in selected {
        let e = &entries[i];
        if e.encrypted {
            report.error(format!("{}: encrypted entries are not supported", e.name));
            continue;
        }
        let Some((mut rel, stripped)) = sanitize(&e.name) else {
            report.warn(format!("{}: unsafe name, skipping", e.name));
            continue;
        };
        if stripped {
            report.warn(format!("{}: removing leading '/' or drive from the name", e.name));
        }
        if cfg.strip_components > 0 {
            if rel.len() <= cfg.strip_components {
                continue;
            }
            rel.drain(..cfg.strip_components);
        }
        if cfg.flat {
            if e.kind == EntryKind::Dir {
                continue;
            }
            rel = rel.split_off(rel.len().saturating_sub(1));
        }
        if rel.is_empty() {
            continue;
        }
        planned.push(Planned { index: i, rel, kind: e.kind });
    }

    // Later duplicates win; earlier copies are skipped.
    let mut last: HashMap<String, usize> = HashMap::new();
    for (pi, p) in planned.iter().enumerate() {
        last.insert(key(&p.rel), pi);
    }
    let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
    let mut kept = Vec::new();
    for (pi, p) in planned.iter().enumerate() {
        let k = key(&p.rel);
        if last[&k] != pi {
            if p.kind != EntryKind::Dir {
                report.warn(format!("{}: duplicate entry, a later copy is used", entries[p.index].name));
            }
            continue;
        }
        for n in 1..p.rel.len() {
            dirs.entry(key(&p.rel[..n])).or_insert_with(|| p.rel[..n].to_vec());
        }
        if p.kind == EntryKind::Dir {
            dirs.entry(k).or_insert_with(|| p.rel.clone());
        }
        kept.push(pi);
    }
    let mut files = Vec::new();
    let mut links = Vec::new();
    let mut dir_entries = Vec::new();
    for pi in kept {
        let p = &planned[pi];
        match p.kind {
            EntryKind::Dir => dir_entries.push(pi),
            _ if dirs.contains_key(&key(&p.rel)) => {
                report.warn(format!("{}: conflicts with a directory of the same name, skipping", entries[p.index].name));
            }
            EntryKind::File => files.push(pi),
            EntryKind::Symlink => links.push(pi),
        }
    }

    let unsafe_dirs = create_dirs(&cfg.dest, dirs, cfg, report, &key);
    let blocked = |rel: &[String]| (1..rel.len()).any(|n| unsafe_dirs.contains(&key(&rel[..n])));

    let size = |pi: &usize| entries[planned[*pi].index].size;
    report.set_total(files.iter().map(size).sum());
    let jobs = schedule(&planned, files, &size);
    let cursor = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..cfg.threads.max(1) {
            s.spawn(|| {
                while let Some(job) = jobs.get(cursor.fetch_add(1, Ordering::Relaxed)) {
                    for &pi in job {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        let p = &planned[pi];
                        if blocked(&p.rel) {
                            report.warn(format!("{}: parent directory is unsafe, skipping", entries[p.index].name));
                            continue;
                        }
                        extract_file(reader, p, cfg, report);
                    }
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::Error::Cancelled);
    }

    for pi in links {
        let p = &planned[pi];
        if blocked(&p.rel) {
            report.warn(format!("{}: parent directory is unsafe, skipping", entries[p.index].name));
            continue;
        }
        extract_link(reader, p, cfg, report);
    }

    let mut dir_entries: Vec<&Planned> = dir_entries.iter().map(|&pi| &planned[pi]).filter(|p| !unsafe_dirs.contains(&key(&p.rel))).collect();
    dir_entries.sort_by_key(|p| std::cmp::Reverse(p.rel.len()));
    for p in dir_entries {
        let path = join(&cfg.dest, &p.rel);
        if let Err(e) = fsmeta::restore_dir(&path, &entries[p.index], cfg.restore) {
            report.warn(format!("{}: cannot restore metadata: {e}", path.display()));
        }
    }
    Ok(())
}

/// Splits files into jobs: big files first (one per job), then small files grouped by directory,
/// so that workers rarely create files in the same directory at the same time.
fn schedule(planned: &[Planned], files: Vec<usize>, size: &dyn Fn(&usize) -> u64) -> Vec<Vec<usize>> {
    const BIG: u64 = 1 << 20;
    const GROUP: usize = 512;
    let (mut big, small): (Vec<usize>, Vec<usize>) = files.into_iter().partition(|pi| size(pi) >= BIG);
    big.sort_by_key(|pi| std::cmp::Reverse(size(pi)));
    let mut jobs: Vec<Vec<usize>> = big.into_iter().map(|pi| vec![pi]).collect();
    let mut by_dir: HashMap<&[String], usize> = HashMap::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for pi in small {
        let rel = &planned[pi].rel;
        let g = *by_dir.entry(&rel[..rel.len() - 1]).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(pi);
    }
    for g in groups {
        jobs.extend(g.chunks(GROUP).map(<[usize]>::to_vec));
    }
    jobs
}

/// Creates all needed directories level by level; returns keys of directories that are unusable.
fn create_dirs(dest: &Path, dirs: HashMap<String, Vec<String>>, cfg: &ExtractConfig, report: &Reporter, key: &(dyn Fn(&[String]) -> String + Sync)) -> HashSet<String> {
    let mut levels: Vec<Vec<Vec<String>>> = Vec::new();
    for rel in dirs.into_values() {
        let d = rel.len();
        if levels.len() < d {
            levels.resize_with(d, Vec::new);
        }
        levels[d - 1].push(rel);
    }
    let bad = Mutex::new(HashSet::new());
    for level in levels {
        let work = |rel: &Vec<String>| {
            if rel.len() > 1 && bad.lock().unwrap().contains(&key(&rel[..rel.len() - 1])) {
                bad.lock().unwrap().insert(key(rel));
                return;
            }
            let path = join(dest, rel);
            if let Err(e) = ensure_dir(&path, cfg.allow_unsafe_symlinks) {
                report.warn(format!("{}: {e}", path.display()));
                bad.lock().unwrap().insert(key(rel));
            }
        };
        if level.len() < 64 || cfg.threads <= 1 {
            level.iter().for_each(work);
        } else {
            let cursor = AtomicUsize::new(0);
            thread::scope(|s| {
                for _ in 0..cfg.threads {
                    s.spawn(|| {
                        while let Some(rel) = level.get(cursor.fetch_add(1, Ordering::Relaxed)) {
                            work(rel);
                        }
                    });
                }
            });
        }
    }
    bad.into_inner().unwrap()
}

fn ensure_dir(path: &Path, allow_symlinks: bool) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(m) if m.file_type().is_symlink() => {
            if allow_symlinks && fs::metadata(path).is_ok_and(|m| m.is_dir()) {
                Ok(())
            } else {
                Err(io::Error::other("is a symbolic link (use --allow-unsafe-symlinks to follow)"))
            }
        }
        Ok(_) => Err(io::Error::other("exists and is not a directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => ensure_dir(path, allow_symlinks),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// Decides what to do with an existing target; returns false to skip the entry.
fn prepare_target(path: &Path, e: &EntryInfo, cfg: &ExtractConfig, report: &Reporter) -> bool {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return true,
        Err(err) => {
            report.error(format!("{}: {err}", path.display()));
            return false;
        }
    };
    if m.is_dir() {
        report.warn(format!("{}: a directory with this name exists, skipping", path.display()));
        return false;
    }
    match cfg.overwrite {
        Overwrite::Never => {
            report.warn(format!("{}: already exists, skipping (use -o to overwrite)", path.display()));
            return false;
        }
        Overwrite::Newer => {
            let existing = m.modified().ok().map(Time::from_system);
            let newer = match (e.mtime, existing) {
                (Some(a), Some(b)) => a.secs > b.secs,
                _ => false,
            };
            if !newer {
                report.info(format!("{}: up to date, skipping", path.display()));
                return false;
            }
        }
        Overwrite::Always => {}
    }
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(err) => {
            report.error(format!("{}: cannot replace: {err}", path.display()));
            false
        }
    }
}

fn extract_file(reader: &dyn ArchiveReader, p: &Planned, cfg: &ExtractConfig, report: &Reporter) {
    let e = &reader.entries()[p.index];
    let path = join(&cfg.dest, &p.rel);
    let create = || OpenOptions::new().write(true).create_new(true).open(&path);
    let file = match create() {
        Ok(f) => f,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            if !prepare_target(&path, e, cfg, report) {
                return;
            }
            match create() {
                Ok(f) => f,
                Err(err) => {
                    report.error(format!("{}: {err}", path.display()));
                    return;
                }
            }
        }
        Err(err) => {
            report.error(format!("{}: {err}", path.display()));
            return;
        }
    };
    let res = (|| -> Result<()> {
        if e.size <= WHOLE_LIMIT && e.compressed_size <= WHOLE_LIMIT {
            let data = reader.read_entry(p.index)?;
            (&file).write_all(&data)?;
        } else {
            let mut r = reader.open_entry(p.index)?;
            let mut w = BufWriter::with_capacity(1 << 20, &file);
            io::copy(&mut r, &mut w)?;
            w.flush()?;
        }
        Ok(())
    })();
    if let Err(err) = res {
        drop(file);
        let _ = fs::remove_file(&path);
        report.error(format!("{}: {err}", e.name));
        return;
    }
    if let Err(err) = fsmeta::restore_file(&file, e, cfg.restore) {
        report.warn(format!("{}: cannot restore metadata: {err}", path.display()));
    }
    drop(file);
    if let Err(err) = fsmeta::restore_path_attrs(&path, e, cfg.restore) {
        report.warn(format!("{}: cannot restore attributes: {err}", path.display()));
    }
    report.bar().inc(e.size);
    report.info(format!("  inflating: {}", path.display()));
}

fn extract_link(reader: &dyn ArchiveReader, p: &Planned, cfg: &ExtractConfig, report: &Reporter) {
    let e = &reader.entries()[p.index];
    let path = join(&cfg.dest, &p.rel);
    let target = match reader.read_entry(p.index) {
        Ok(t) => t,
        Err(err) => {
            report.error(format!("{}: {err}", e.name));
            return;
        }
    };
    if !cfg.allow_unsafe_symlinks && link_escapes(&p.rel, &target) {
        report.warn(format!(
            "{}: link target {:?} points outside the destination, skipping (use --allow-unsafe-symlinks)",
            e.name,
            String::from_utf8_lossy(&target)
        ));
        return;
    }
    if !prepare_target(&path, e, cfg, report) {
        return;
    }
    if let Err(err) = fsmeta::symlink(&target, &path) {
        if cfg!(windows) {
            report.warn(format!("{}: cannot create symlink ({err}); writing the link target as a file", path.display()));
            if let Err(err) = fs::write(&path, &target) {
                report.error(format!("{}: {err}", path.display()));
            }
        } else {
            report.error(format!("{}: {err}", path.display()));
        }
        return;
    }
    report.info(format!("    linking: {} -> {}", path.display(), String::from_utf8_lossy(&target)));
}

/// Decodes the selected entries in parallel and verifies their checksums.
pub fn test(reader: &dyn ArchiveReader, selected: &[usize], threads: usize, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    let entries = reader.entries();
    let mut items: Vec<usize> = selected.iter().copied().filter(|&i| entries[i].kind != EntryKind::Dir).collect();
    report.set_total(items.iter().map(|&i| entries[i].size).sum());
    items.sort_by_key(|&i| std::cmp::Reverse(entries[i].size.max(1 << 20)));
    let cursor = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                while let Some(&i) = items.get(cursor.fetch_add(1, Ordering::Relaxed)) {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let e = &entries[i];
                    if e.encrypted {
                        report.error(format!("{}: encrypted entries are not supported", e.name));
                        continue;
                    }
                    let res = if e.size <= WHOLE_LIMIT && e.compressed_size <= WHOLE_LIMIT {
                        reader.read_entry(i).map(|_| ())
                    } else {
                        reader.open_entry(i).and_then(|mut r| io::copy(&mut r, &mut io::sink()).map(|_| ()).map_err(Into::into))
                    };
                    match res {
                        Ok(()) => report.info(format!("    testing: {:<40} OK", e.name)),
                        Err(err) => report.error(format!("{}: {err}", e.name)),
                    }
                    report.bar().inc(e.size);
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::Error::Cancelled);
    }
    Ok(())
}

/// Writes the contents of the selected file entries to `out` in archive order.
pub fn to_writer(reader: &dyn ArchiveReader, selected: &[usize], out: &mut dyn Write, report: &Reporter) -> Result<()> {
    for &i in selected {
        let e = &reader.entries()[i];
        if e.kind != EntryKind::File {
            continue;
        }
        let mut r = match reader.open_entry(i) {
            Ok(r) => r,
            Err(err) => {
                report.error(format!("{}: {err}", e.name));
                continue;
            }
        };
        if let Err(err) = io::copy(&mut r, out) {
            if err.kind() == io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            report.error(format!("{}: {err}", e.name));
        }
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn sanitize_rejects_traversal_and_strips_roots() {
        assert_eq!(sanitize("a/b/c"), Some((s(&["a", "b", "c"]), false)));
        assert_eq!(sanitize("./a//b/"), Some((s(&["a", "b"]), false)));
        assert_eq!(sanitize("/etc/passwd"), Some((s(&["etc", "passwd"]), true)));
        assert_eq!(sanitize("C:\\x\\y"), Some((s(&["x", "y"]), true)));
        assert_eq!(sanitize("a/../../b"), None);
        assert_eq!(sanitize("..\\evil"), None);
        assert_eq!(sanitize("a\0b"), None);
    }

    #[test]
    fn link_escape_detection() {
        assert!(!link_escapes(&s(&["a", "link"]), b"target"));
        assert!(!link_escapes(&s(&["a", "link"]), b"../b/c"));
        assert!(link_escapes(&s(&["a", "link"]), b"../../x"));
        assert!(link_escapes(&s(&["link"]), b"../x"));
        assert!(link_escapes(&s(&["link"]), b"/etc/passwd"));
        assert!(link_escapes(&s(&["link"]), b"C:\\Windows"));
        assert!(!link_escapes(&s(&["d", "link"]), b"./x/../y"));
    }
}
