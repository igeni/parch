mod common;

use std::fs;

use common::*;

const BIG: [&str; 4] = ["--big-file", "256K", "--chunk-size", "64K"];

#[test]
fn create_extract_roundtrip_default() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, true);
    let zip = t.path().join("a.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("src").args(BIG).current_dir(t.path()).assert().success();
    let out = t.path().join("out");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&out).assert().success();
    assert_same_tree(&src, &out.join("src"));
    parch().args(["t", "-q"]).arg(&zip).assert().success();
}

#[test]
fn roundtrip_with_every_thread_count_and_levels() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, true);
    for (j, level) in [("1", "1"), ("3", "6"), ("8", "9"), ("2", "12"), ("4", "0")] {
        let zip = t.path().join(format!("j{j}l{level}.zip"));
        parch().args(["c", "-q", "-j", j, "-l", level]).arg(&zip).arg("src").args(BIG).current_dir(t.path()).assert().success();
        let out = t.path().join(format!("out{j}{level}"));
        parch().args(["x", "-q", "-j", j]).arg(&zip).arg("-C").arg(&out).assert().success();
        assert_same_tree(&src, &out.join("src"));
    }
}

#[test]
fn sorted_output_is_identical_for_any_thread_count() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, true);
    let mut archives = Vec::new();
    for j in ["1", "2", "8"] {
        let zip = t.path().join(format!("s{j}.zip"));
        parch().args(["c", "-q", "--sort", "-j", j]).arg(&zip).arg("src").args(BIG).current_dir(t.path()).assert().success();
        archives.push(fs::read(&zip).unwrap());
    }
    assert_eq!(archives[0], archives[1]);
    assert_eq!(archives[0], archives[2]);
}

#[test]
fn reproducible_ignores_metadata_noise() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, false);
    let a = t.path().join("r1.zip");
    let b = t.path().join("r2.zip");
    parch().args(["c", "-q", "--reproducible", "-j", "1"]).arg(&a).arg("src").current_dir(t.path()).env("SOURCE_DATE_EPOCH", "1700000000").assert().success();
    // Touch atime/ctime and change the owner-independent bits only through a fresh copy.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let _ = fs::read(src.join("a.txt"));
    parch().args(["c", "-q", "--reproducible", "-j", "6"]).arg(&b).arg("src").current_dir(t.path()).env("SOURCE_DATE_EPOCH", "1700000000").assert().success();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn files_compress_and_store_as_expected() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, true);
    let zip = t.path().join("a.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("src").args(BIG).current_dir(t.path()).assert().success();
    let mut z = zip::ZipArchive::new(fs::File::open(&zip).unwrap()).unwrap();
    let method = |z: &mut zip::ZipArchive<fs::File>, n: &str| z.by_name(n).unwrap().compression();
    assert_eq!(method(&mut z, "src/sub/text.txt"), zip::CompressionMethod::Deflated);
    assert_eq!(method(&mut z, "src/sub/noise.bin"), zip::CompressionMethod::Stored);
    assert_eq!(method(&mut z, "src/sub/photo.jpg"), zip::CompressionMethod::Stored);
    assert_eq!(method(&mut z, "src/empty.txt"), zip::CompressionMethod::Stored);
    assert_eq!(method(&mut z, "src/big/big.dat"), zip::CompressionMethod::Deflated);
}

#[test]
fn include_exclude_and_directory_options() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, false);
    let zip = t.path().join("f.zip");
    parch().args(["c", "-q", "-C"]).arg(&src).arg(&zip).args([".", "-x", "*.bin", "-x", "deep"]).assert().success();
    let names: Vec<String> = zip_crate_read(&zip).into_iter().map(|e| e.0).collect();
    assert!(names.contains(&"a.txt".to_string()), "{names:?}");
    assert!(!names.iter().any(|n| n.ends_with(".bin")));
    assert!(!names.iter().any(|n| n.contains("deep")));
    assert!(!names.iter().any(|n| n.starts_with("./") || n.starts_with('/')));

    let zip2 = t.path().join("i.zip");
    parch().args(["c", "-q", "-D", "-i", "*.txt"]).arg(&zip2).arg("src").current_dir(t.path()).assert().success();
    let entries = zip_crate_read(&zip2);
    assert!(entries.iter().all(|e| e.0.ends_with(".txt") && !e.1), "{:?}", entries.iter().map(|e| &e.0).collect::<Vec<_>>());
}

#[test]
fn overlapping_inputs_are_deduplicated() {
    let t = tmp();
    sample_tree(&t.path().join("src"), false);
    let zip = t.path().join("d.zip");
    parch().args(["c", "-q"]).arg(&zip).args(["src", "src/sub", "src/a.txt", "src"]).current_dir(t.path()).assert().success();
    let mut names: Vec<String> = zip_crate_read(&zip).into_iter().map(|e| e.0).collect();
    let n = names.len();
    names.dedup();
    assert_eq!(names.len(), n);
}

#[test]
fn central_directory_is_sorted_by_name() {
    let t = tmp();
    sample_tree(&t.path().join("src"), false);
    let zip = t.path().join("o.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    let names: Vec<String> = zip_crate_read(&zip).into_iter().map(|e| e.0).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn extract_overwrite_policies_and_selection() {
    let t = tmp();
    let src = t.path().join("src");
    sample_tree(&src, false);
    let zip = t.path().join("a.zip");
    parch().args(["c", "-q"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    let out = t.path().join("out");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&out).args(["src/a.txt"]).assert().success();
    assert!(out.join("src/a.txt").exists());
    assert!(!out.join("src/sub").exists());

    fs::write(out.join("src/a.txt"), b"local change").unwrap();
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&out).args(["src/a.txt"]).assert().code(1);
    assert_eq!(fs::read(out.join("src/a.txt")).unwrap(), b"local change");
    parch().args(["x", "-q", "-u"]).arg(&zip).arg("-C").arg(&out).args(["src/a.txt"]).assert().success();
    assert_eq!(fs::read(out.join("src/a.txt")).unwrap(), b"local change");
    parch().args(["x", "-q", "-o"]).arg(&zip).arg("-C").arg(&out).args(["src/a.txt"]).assert().success();
    assert_eq!(fs::read(out.join("src/a.txt")).unwrap(), b"hello world\n");

    let flat = t.path().join("flat");
    parch().args(["x", "-q", "--flat"]).arg(&zip).arg("-C").arg(&flat).args(["*.txt"]).assert().success();
    assert!(flat.join("x.txt").exists() && flat.join("a.txt").exists());

    // Stripping makes `sub/uplink -> ../a.txt` point outside the destination: skipped with a warning.
    let strip = t.path().join("strip");
    let code = parch().args(["x", "-q", "--strip-components", "2"]).arg(&zip).arg("-C").arg(&strip).output().unwrap().status.code();
    assert_eq!(code, Some(if cfg!(unix) { 1 } else { 0 }));
    assert!(strip.join("deep/deeper/x.txt").exists());
    assert!(!strip.join("uplink").exists());
}

#[test]
fn list_output_contains_entries() {
    let t = tmp();
    sample_tree(&t.path().join("src"), false);
    let zip = t.path().join("a.zip");
    parch().args(["c", "-q", "--comment", "hello comment"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    let out = parch().arg("l").arg(&zip).output().unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("hello comment"));
    assert!(s.contains("src/юникод/файл.txt"));
    let out = parch().args(["l", "-v"]).arg(&zip).output().unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("Defl:N") && s.contains("Stored"));
}

#[test]
fn archive_never_contains_itself() {
    let t = tmp();
    write(&t.path().join("w/a.txt"), b"a");
    write(&t.path().join("w/out.zip"), b"stale archive from before");
    parch().args(["c", "-q", "out.zip", "."]).current_dir(t.path().join("w")).assert().success();
    let names: Vec<String> = zip_crate_read(&t.path().join("w/out.zip")).into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["a.txt"]);
    let leftovers: Vec<_> = fs::read_dir(t.path().join("w")).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(leftovers.is_empty());
}

#[cfg(unix)]
#[test]
fn special_and_unreadable_files_are_skipped_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let t = tmp();
    write(&t.path().join("s/ok.txt"), b"ok");
    write(&t.path().join("s/secret.txt"), b"secret");
    let fifo = t.path().join("s/pipe");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    fs::set_permissions(t.path().join("s/secret.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::File::open(t.path().join("s/secret.txt")).is_ok();
    let zip = t.path().join("s.zip");
    let out = parch().args(["c"]).arg(&zip).arg("s").current_dir(t.path()).output().unwrap();
    fs::set_permissions(t.path().join("s/secret.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("pipe"), "{stderr}");
    let names: Vec<String> = zip_crate_read(&zip).into_iter().map(|e| e.0).collect();
    assert!(names.contains(&"s/ok.txt".to_string()));
    assert!(!names.contains(&"s/pipe".to_string()));
    if !readable {
        assert!(!names.contains(&"s/secret.txt".to_string()));
        assert!(stderr.contains("secret.txt"), "{stderr}");
    }
}
