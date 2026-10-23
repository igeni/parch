//! Hostile and damaged archives: nothing is written outside the destination, nothing panics.

mod common;

use std::fs;
use std::io::Write;

use common::*;
use proptest::prelude::*;
use zip::write::SimpleFileOptions;

fn hostile(path: &std::path::Path, entries: &[(&str, &[u8])], links: &[(&str, &str)]) {
    let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in entries {
        w.start_file(*name, SimpleFileOptions::default()).unwrap();
        w.write_all(data).unwrap();
    }
    for (name, target) in links {
        w.add_symlink(*name, *target, SimpleFileOptions::default()).unwrap();
    }
    w.finish().unwrap();
}

#[test]
fn traversal_names_are_neutralized() {
    let t = tmp();
    let zip = t.path().join("evil.zip");
    hostile(&zip, &[("../escape.txt", b"x"), ("a/../../escape2.txt", b"x"), ("/abs/path.txt", b"abs"), ("C:\\win\\path.txt", b"win"), ("ok.txt", b"ok")], &[]);
    let dest = t.path().join("dest");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&dest).assert().code(1);
    assert!(!t.path().join("escape.txt").exists());
    assert!(!t.path().join("escape2.txt").exists());
    assert_eq!(fs::read(dest.join("ok.txt")).unwrap(), b"ok");
    assert_eq!(fs::read(dest.join("abs/path.txt")).unwrap(), b"abs");
    assert_eq!(fs::read(dest.join("win/path.txt")).unwrap(), b"win");
}

#[cfg(unix)]
#[test]
fn symlink_escapes_are_blocked() {
    let t = tmp();
    let outside = t.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let zip = t.path().join("links.zip");
    // A link to the outside followed by a file "through" the link.
    hostile(&zip, &[("evil/pwned.txt", b"pwned")], &[("evil", outside.to_str().unwrap()), ("rel", "../../outside"), ("fine", "ok/target")]);
    let dest = t.path().join("dest");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&dest).assert().code(1);
    assert!(!outside.join("pwned.txt").exists());
    assert!(fs::symlink_metadata(dest.join("rel")).is_err());
    assert_eq!(fs::read_link(dest.join("fine")).unwrap().to_str(), Some("ok/target"));

    // A symlinked directory that already exists in the destination is not written through.
    let dest2 = t.path().join("dest2");
    fs::create_dir_all(&dest2).unwrap();
    std::os::unix::fs::symlink(&outside, dest2.join("evil")).unwrap();
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&dest2).arg("evil/*").assert().code(1);
    assert!(!outside.join("pwned.txt").exists());
}

#[test]
fn duplicate_and_conflicting_names() {
    let t = tmp();
    let zip = t.path().join("dup.zip");
    hostile(&zip, &[("dup1.txt", b"first"), ("dup2.txt", b"second"), ("x", b"file"), ("x/inner.txt", b"inner")], &[]);
    // The zip crate refuses duplicate names; rename the second entry in place.
    let mut bytes = fs::read(&zip).unwrap();
    for i in 0..bytes.len() - 8 {
        if &bytes[i..i + 8] == b"dup2.txt" {
            bytes[i + 3] = b'1';
        }
    }
    fs::write(&zip, bytes).unwrap();
    let dest = t.path().join("dest");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&dest).assert().code(1);
    assert_eq!(fs::read(dest.join("dup1.txt")).unwrap(), b"second");
    assert_eq!(fs::read(dest.join("x/inner.txt")).unwrap(), b"inner");
}

#[test]
fn corrupt_data_is_detected() {
    let t = tmp();
    write(&t.path().join("f.txt"), &text(100_000, 1));
    let zip = t.path().join("c.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("f.txt").current_dir(t.path()).assert().success();
    let mut bytes = fs::read(&zip).unwrap();
    let mid = 30 + 5 + 9 + 15 + 500;
    bytes[mid] ^= 0x55;
    fs::write(&zip, &bytes).unwrap();
    parch().args(["t", "-q"]).arg(&zip).assert().code(2);
    let dest = t.path().join("dest");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&dest).assert().code(2);
    assert!(!dest.join("f.txt").exists(), "a file that failed verification must be removed");
}

#[test]
fn not_an_archive() {
    let t = tmp();
    let p = t.path().join("junk.zip");
    fs::write(&p, b"this is not a zip archive at all").unwrap();
    parch().args(["l"]).arg(&p).assert().code(2);
    fs::write(&p, b"").unwrap();
    parch().args(["t"]).arg(&p).assert().code(2);
    parch().args(["t"]).arg(t.path().join("missing.zip")).assert().code(2);
}

#[test]
fn prefixed_archive_is_readable() {
    let t = tmp();
    write(&t.path().join("f.txt"), b"payload");
    let zip = t.path().join("p.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("f.txt").current_dir(t.path()).assert().success();
    let mut sfx = b"#!/bin/sh\necho self-extracting stub\nexit 0\n".to_vec();
    sfx.extend(fs::read(&zip).unwrap());
    let s = t.path().join("sfx.zip");
    fs::write(&s, sfx).unwrap();
    parch().args(["t", "-q"]).arg(&s).assert().success();
    let out = parch().args(["x", "-p"]).arg(&s).output().unwrap();
    assert_eq!(out.stdout, b"payload");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, .. ProptestConfig::default() })]

    #[test]
    fn damaged_archives_never_crash(flips in prop::collection::vec((0usize..4096, any::<u8>()), 1..6), cut in 0usize..4096) {
        let t = tmp();
        write(&t.path().join("d/a.txt"), &text(3000, 2));
        write(&t.path().join("d/b.bin"), &noise(700, 3));
        let zip = t.path().join("z.zip");
        parch().args(["c", "-q"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();
        let mut bytes = fs::read(&zip).unwrap();
        for (pos, v) in flips {
            let n = bytes.len();
            bytes[pos % n] ^= v | 1;
        }
        if cut % 3 == 0 {
            let n = bytes.len();
            bytes.truncate(n - (cut % n));
        }
        fs::write(&zip, &bytes).unwrap();
        for args in [&["t", "-q"][..], &["l"], &["x", "-q", "-C", "out"]] {
            let st = parch().args(args).arg(&zip).current_dir(t.path()).output().unwrap().status;
            prop_assert!(matches!(st.code(), Some(0..=2)), "{args:?} exited with {st:?}");
        }
    }
}
