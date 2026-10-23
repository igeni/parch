//! Heavy ZIP64 checks (several GiB of disk): `cargo test -- --ignored`.

mod common;

use std::fs;
use std::process::Command;

use common::*;

const GIB: u64 = 1 << 30;

fn sparse(path: &std::path::Path, len: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let f = fs::File::create(path).unwrap();
    f.set_len(len).unwrap();
}

fn unzip_test(zip: &std::path::Path) {
    if tool("unzip").is_some() {
        let out = Command::new("unzip").arg("-tq").arg(zip).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    }
}

#[test]
#[ignore]
fn entry_larger_than_4gib() {
    let t = tmp();
    sparse(&t.path().join("d/huge.bin"), 4 * GIB + 12_345);
    write(&t.path().join("d/after.txt"), b"after");
    let zip = t.path().join("huge.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();
    parch().args(["t", "-q"]).arg(&zip).assert().success();
    unzip_test(&zip);
    let z = zip::ZipArchive::new(fs::File::open(&zip).unwrap()).unwrap();
    let idx = z.index_for_name("d/huge.bin").unwrap();
    let mut z = z;
    assert_eq!(z.by_index(idx).unwrap().size(), 4 * GIB + 12_345);
}

#[test]
#[ignore]
fn archive_larger_than_4gib_with_modifications() {
    let t = tmp();
    sparse(&t.path().join("d/a_big.bin"), 4 * GIB + 100);
    write(&t.path().join("d/z_after.txt"), b"beyond 4 GiB");
    let zip = t.path().join("big.zip");
    parch().args(["c", "-q", "-0", "--sort"]).arg(&zip).arg("d").current_dir(t.path()).assert().success();
    assert!(fs::metadata(&zip).unwrap().len() > 4 * GIB);
    parch().args(["t", "-q"]).arg(&zip).assert().success();
    unzip_test(&zip);
    let read = |name: &str| zip_crate_read(&zip).into_iter().find(|e| e.0 == name).unwrap().2;
    assert_eq!(read("d/z_after.txt"), b"beyond 4 GiB");

    write(&t.path().join("d/zz_added.txt"), b"added later");
    parch().args(["a", "-q"]).arg(&zip).arg("d/zz_added.txt").current_dir(t.path()).assert().success();
    parch().args(["t", "-q"]).arg(&zip).assert().success();
    unzip_test(&zip);
    assert_eq!(read("d/zz_added.txt"), b"added later");

    parch().args(["d", "-q"]).arg(&zip).arg("d/a_big.bin").assert().success();
    assert!(fs::metadata(&zip).unwrap().len() < GIB);
    parch().args(["t", "-q"]).arg(&zip).assert().success();
    assert_eq!(read("d/z_after.txt"), b"beyond 4 GiB");
}
