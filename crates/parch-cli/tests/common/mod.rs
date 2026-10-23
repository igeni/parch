#![allow(dead_code)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, SystemTime};

use assert_cmd::Command;

/// Temporary directory inside `target/` (tests never write outside the project).
pub fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("t").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

pub fn parch() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_parch"));
    c.env_remove("PARCH_THREADS").env_remove("SOURCE_DATE_EPOCH");
    c
}

pub fn write(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

/// Deterministic pseudo-random bytes.
pub fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Compressible text of `n` bytes.
pub fn text(n: usize, seed: u64) -> Vec<u8> {
    let words = ["alpha ", "beta ", "gamma ", "delta\n", "epsilon ", "zeta "];
    let r = noise(n / 4 + 1, seed);
    let mut v = Vec::with_capacity(n + 8);
    let mut i = 0;
    while v.len() < n {
        v.extend_from_slice(words[r[i % r.len()] as usize % words.len()].as_bytes());
        i += 1;
    }
    v.truncate(n);
    v
}

fn set_mtime(path: &Path, secs: u64) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_times(fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))).unwrap();
}

/// A tree with text, binary, empty, nested, unicode, executable and symlinked entries.
/// `big` adds a file that is compressed in parallel chunks with `--big-file 256K`.
pub fn sample_tree(root: &Path, big: bool) {
    write(&root.join("a.txt"), b"hello world\n");
    write(&root.join("empty.txt"), b"");
    write(&root.join("sub/text.txt"), &text(200_000, 1));
    write(&root.join("sub/noise.bin"), &noise(100_000, 2));
    write(&root.join("sub/deep/deeper/x.txt"), &text(5_000, 3));
    write(&root.join("sub/photo.jpg"), &text(50_000, 4));
    write(&root.join("юникод/файл.txt"), "Привет, мир\n".as_bytes());
    write(&root.join("spaces in name/f g.txt"), b"spaces");
    fs::create_dir_all(root.join("empty_dir")).unwrap();
    if big {
        let mut b = text(1_500_000, 5);
        b.extend(noise(300_000, 6));
        b.extend(text(1_200_000, 7));
        write(&root.join("big/big.dat"), &b);
    }
    for (i, f) in ["a.txt", "sub/text.txt", "sub/noise.bin", "юникод/файл.txt"].iter().enumerate() {
        set_mtime(&root.join(f), 1_600_000_000 + i as u64 * 1001);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        write(&root.join("run.sh"), b"#!/bin/sh\necho hi\n");
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("text.txt", root.join("sub/link")).unwrap();
        std::os::unix::fs::symlink("../a.txt", root.join("sub/uplink")).unwrap();
    }
}

fn mtime_secs(md: &fs::Metadata) -> i64 {
    md.modified().unwrap().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Asserts that `b` mirrors `a`: same entries, contents, symlinks, permissions and mtimes.
pub fn assert_same_tree(a: &Path, b: &Path) {
    let mut seen = 0;
    walk(a, &mut |rel| {
        seen += 1;
        let (pa, pb) = (a.join(rel), b.join(rel));
        let ma = fs::symlink_metadata(&pa).unwrap();
        let mb = fs::symlink_metadata(&pb).unwrap_or_else(|e| panic!("{}: {e}", pb.display()));
        assert_eq!(ma.file_type().is_dir(), mb.file_type().is_dir(), "{rel:?}");
        assert_eq!(ma.file_type().is_symlink(), mb.file_type().is_symlink(), "{rel:?}");
        if ma.file_type().is_symlink() {
            assert_eq!(fs::read_link(&pa).unwrap(), fs::read_link(&pb).unwrap(), "{rel:?}");
            return;
        }
        if ma.is_file() {
            assert_eq!(fs::read(&pa).unwrap(), fs::read(&pb).unwrap(), "content of {rel:?}");
            assert!((mtime_secs(&ma) - mtime_secs(&mb)).abs() <= 1, "mtime of {rel:?}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(ma.permissions().mode() & 0o777, mb.permissions().mode() & 0o777, "mode of {rel:?}");
        }
    });
    let mut other = 0;
    walk(b, &mut |_| other += 1);
    assert_eq!(seen, other, "entry count differs");
}

pub fn walk(root: &Path, f: &mut dyn FnMut(&Path)) {
    fn go(root: &Path, rel: &Path, f: &mut dyn FnMut(&Path)) {
        let mut names: Vec<_> = fs::read_dir(root.join(rel)).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        for n in names {
            let r = rel.join(&n);
            f(&r);
            if fs::symlink_metadata(root.join(&r)).unwrap().is_dir() {
                go(root, &r, f);
            }
        }
    }
    go(root, Path::new(""), f);
}

/// Entries of an archive read with the `zip` crate: name → (is_dir, contents).
pub fn zip_crate_read(archive: &Path) -> Vec<(String, bool, Vec<u8>, Option<u32>)> {
    let mut z = zip::ZipArchive::new(fs::File::open(archive).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let name = f.name().unwrap().into_owned();
            let mut data = Vec::new();
            f.read_to_end(&mut data).unwrap();
            (name, f.is_dir(), data, f.unix_mode())
        })
        .collect()
}

pub fn tool(name: &str) -> Option<PathBuf> {
    let out = StdCommand::new("sh").arg("-c").arg(format!("command -v {name}")).output().ok()?;
    let p = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !p.is_empty()).then(|| PathBuf::from(p))
}
