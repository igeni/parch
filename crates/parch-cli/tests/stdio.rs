mod common;

use std::fs;

use common::*;

#[test]
fn archive_to_stdout_and_back_through_stdin() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, true);
    // Big files are chunked: on a pipe they get data descriptors.
    let out = parch().args(["c", "-q", "-", "src", "--big-file", "256K", "--chunk-size", "64K"]).current_dir(t.path()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let zip = t.path().join("piped.zip");
    fs::write(&zip, &out.stdout).unwrap();

    parch().args(["t", "-q"]).arg(&zip).assert().success();
    let entries = zip_crate_read(&zip);
    let big = entries.iter().find(|e| e.0 == "src/big/big.dat").unwrap();
    assert_eq!(big.2, fs::read(src.join("big/big.dat")).unwrap());

    let dest = t.path().join("dest");
    parch().args(["x", "-q", "-", "-C"]).arg(&dest).write_stdin(out.stdout.clone()).assert().success();
    assert_same_tree(&src, &dest.join("src"));

    let listing = parch().args(["l", "-"]).write_stdin(out.stdout).output().unwrap();
    assert!(String::from_utf8(listing.stdout).unwrap().contains("src/big/big.dat"));

    if tool("unzip").is_some() {
        assert!(std::process::Command::new("unzip").arg("-tq").arg(&zip).status().unwrap().success());
    }
}

#[test]
fn file_list_from_stdin_and_contents_to_stdout() {
    let t = tmp();
    write(&t.path().join("one.txt"), b"1");
    write(&t.path().join("dir/two.txt"), b"22");
    write(&t.path().join("dir/three.txt"), b"333");
    let zip = t.path().join("l.zip");
    parch().args(["c", "-q", "-T", "-"]).arg(&zip).current_dir(t.path()).write_stdin("one.txt\ndir/two.txt\n").assert().success();
    let names: Vec<String> = zip_crate_read(&zip).into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["dir/two.txt", "one.txt"]);

    let zip0 = t.path().join("l0.zip");
    parch().args(["c", "-q", "--null", "-T", "-"]).arg(&zip0).current_dir(t.path()).write_stdin("one.txt\0dir/three.txt\0").assert().success();
    assert_eq!(zip_crate_read(&zip0).len(), 2);

    let out = parch().args(["x", "-p"]).arg(&zip).arg("*two.txt").output().unwrap();
    assert_eq!(out.stdout, b"22");
}

#[test]
fn refuses_to_update_streams() {
    let t = tmp();
    write(&t.path().join("f"), b"x");
    parch().args(["a", "-", "f"]).current_dir(t.path()).assert().code(2);
}
