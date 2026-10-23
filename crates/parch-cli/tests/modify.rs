mod common;

use std::fs;
use std::time::{Duration, SystemTime};

use common::*;

fn names(zip: &std::path::Path) -> Vec<String> {
    zip_crate_read(zip).into_iter().map(|e| e.0).collect()
}

fn content(zip: &std::path::Path, name: &str) -> Vec<u8> {
    zip_crate_read(zip).into_iter().find(|e| e.0 == name).unwrap_or_else(|| panic!("{name} missing")).2
}

fn touch(path: &std::path::Path, secs_from_now: i64) {
    let t = if secs_from_now >= 0 {
        SystemTime::now() + Duration::from_secs(secs_from_now as u64)
    } else {
        SystemTime::now() - Duration::from_secs(secs_from_now.unsigned_abs())
    };
    fs::File::options().write(true).open(path).unwrap().set_times(fs::FileTimes::new().set_modified(t)).unwrap();
}

#[test]
fn add_replaces_and_appends() {
    let t = tmp();
    sample_tree(&t.path().join("src"), false);
    let zip = t.path().join("a.zip");
    parch().args(["c", "-q", "--comment", "keep me"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    let before = names(&zip);

    fs::write(t.path().join("src/a.txt"), b"changed").unwrap();
    write(&t.path().join("src/new/file.txt"), b"new file");
    parch().args(["a", "-q"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    let after = names(&zip);
    assert_eq!(content(&zip, "src/a.txt"), b"changed");
    assert_eq!(content(&zip, "src/new/file.txt"), b"new file");
    assert!(before.iter().all(|n| after.contains(n)));
    assert_eq!(after.len(), before.len() + 2);
    assert_eq!(after.iter().filter(|n| *n == "src/a.txt").count(), 1);
    let listing = String::from_utf8(parch().arg("l").arg(&zip).output().unwrap().stdout).unwrap();
    assert!(listing.contains("keep me"));
    parch().args(["t", "-q"]).arg(&zip).assert().success();
}

#[test]
fn update_and_freshen_respect_times() {
    let t = tmp();
    write(&t.path().join("d/old.txt"), b"old");
    write(&t.path().join("d/keep.txt"), b"keep");
    touch(&t.path().join("d/old.txt"), -1000);
    touch(&t.path().join("d/keep.txt"), -1000);
    let zip = t.path().join("u.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();

    fs::write(t.path().join("d/old.txt"), b"newer").unwrap();
    touch(&t.path().join("d/old.txt"), 0);
    write(&t.path().join("d/added.txt"), b"added");
    parch().args(["a", "-q", "-f"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();
    assert_eq!(content(&zip, "d/old.txt"), b"newer");
    assert!(!names(&zip).contains(&"d/added.txt".to_string()));

    fs::write(t.path().join("d/keep.txt"), b"modified but older").unwrap();
    touch(&t.path().join("d/keep.txt"), -5000);
    parch().args(["a", "-q", "-u"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();
    assert_eq!(content(&zip, "d/keep.txt"), b"keep");
    assert_eq!(content(&zip, "d/added.txt"), b"added");

    let mtime = fs::metadata(&zip).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    parch().args(["a", "-q", "-u"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();
    assert_eq!(fs::metadata(&zip).unwrap().modified().unwrap(), mtime, "nothing to update must not rewrite");
}

#[test]
fn add_to_missing_archive_creates_it() {
    let t = tmp();
    write(&t.path().join("f.txt"), b"x");
    let zip = t.path().join("new.zip");
    parch().args(["a", "-q"]).arg(&zip).arg("f.txt").current_dir(t.path()).assert().success();
    assert_eq!(names(&zip), vec!["f.txt"]);
}

#[test]
fn delete_by_pattern() {
    let t = tmp();
    sample_tree(&t.path().join("src"), true);
    let zip = t.path().join("d.zip");
    parch().args(["c", "-q", "--big-file", "256K"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    let before = names(&zip);
    parch().args(["d", "-q"]).arg(&zip).args(["src/sub/*", "*.sh"]).assert().success();
    let after = names(&zip);
    assert!(after.iter().all(|n| !n.starts_with("src/sub/") && !n.ends_with(".sh")), "{after:?}");
    assert!(after.contains(&"src/a.txt".to_string()));
    assert!(after.contains(&"src/big/big.dat".to_string()));
    assert!(after.len() < before.len());
    parch().args(["t", "-q"]).arg(&zip).assert().success();
    assert_eq!(content(&zip, "src/big/big.dat"), fs::read(t.path().join("src/big/big.dat")).unwrap());

    let size = fs::metadata(&zip).unwrap().len();
    parch().args(["d", "-q"]).arg(&zip).args(["nothing-matches"]).assert().code(1);
    assert_eq!(fs::metadata(&zip).unwrap().len(), size);
}

#[test]
fn modify_preserves_foreign_entries_verbatim() {
    use std::io::Write;
    let t = tmp();
    let zip = t.path().join("f.zip");
    {
        let mut w = zip::ZipWriter::new(fs::File::create(&zip).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Bzip2);
        w.start_file("bz/data.txt", opts).unwrap();
        w.write_all(&text(50_000, 3)).unwrap();
        w.start_file("gone.txt", opts).unwrap();
        w.write_all(b"bye").unwrap();
        w.finish().unwrap();
    }
    write(&t.path().join("added.txt"), b"added");
    parch().args(["a", "-q"]).arg(&zip).arg("added.txt").current_dir(t.path()).assert().success();
    parch().args(["d", "-q"]).arg(&zip).arg("gone.txt").assert().success();
    let mut z = zip::ZipArchive::new(fs::File::open(&zip).unwrap()).unwrap();
    assert_eq!(z.len(), 2);
    assert_eq!(z.by_name("bz/data.txt").unwrap().compression(), zip::CompressionMethod::Bzip2);
    assert_eq!(content(&zip, "bz/data.txt"), text(50_000, 3));
    assert_eq!(content(&zip, "added.txt"), b"added");
}
