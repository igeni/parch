//! Interoperability: archives written by parch are read by other implementations and vice versa.

mod common;

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::process::Command;

use common::*;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

const BIG: [&str; 4] = ["--big-file", "256K", "--chunk-size", "64K"];

fn make_archive(t: &tempfile::TempDir, extra: &[&str]) -> std::path::PathBuf {
    sample_tree(&t.path().join("src"), true);
    let zip = t.path().join("p.zip");
    parch().args(["c", "-q"]).args(extra).arg(&zip).arg("src").args(BIG).current_dir(t.path()).assert().success();
    zip
}

#[test]
fn zip_crate_reads_parch_archives() {
    for extra in [&[][..], &["--zip64", "always"], &["-0"], &["-X"], &["--reproducible"]] {
        let t = tmp();
        let zip = make_archive(&t, extra);
        let src = t.path();
        for (name, is_dir, data, mode) in zip_crate_read(&zip) {
            let path = src.join(&name);
            let md = fs::symlink_metadata(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            if is_dir {
                assert!(md.is_dir(), "{name}");
            } else if md.file_type().is_symlink() {
                assert_eq!(data, fs::read_link(&path).unwrap().to_str().unwrap().as_bytes(), "{name}");
                assert_eq!(mode.unwrap() & 0o170000, 0o120000, "{name}");
            } else {
                assert_eq!(data, fs::read(&path).unwrap(), "{name} with {extra:?}");
            }
        }
    }
}

#[test]
fn parch_reads_zip_crate_archives_with_all_methods() {
    let t = tmp();
    let zip = t.path().join("methods.zip");
    let payload = text(300_000, 9);
    let small = b"tiny".to_vec();
    // The zip crate cannot write LZMA; LZMA input is covered by the Python test below.
    let methods = [CompressionMethod::Stored, CompressionMethod::Deflated, CompressionMethod::Bzip2, CompressionMethod::Zstd, CompressionMethod::Xz, CompressionMethod::Ppmd];
    {
        let mut w = zip::ZipWriter::new(fs::File::create(&zip).unwrap());
        for (i, m) in methods.iter().enumerate() {
            let opts = SimpleFileOptions::default().compression_method(*m).unix_permissions(0o640);
            w.start_file(format!("m{i}/payload.txt"), opts).unwrap();
            w.write_all(&payload).unwrap();
            w.start_file(format!("m{i}/small.txt"), opts).unwrap();
            w.write_all(&small).unwrap();
        }
        w.add_directory("emptydir/", SimpleFileOptions::default()).unwrap();
        if cfg!(unix) {
            w.add_symlink("m0/link", "payload.txt", SimpleFileOptions::default()).unwrap();
        }
        w.finish().unwrap();
    }
    parch().args(["t", "-q"]).arg(&zip).assert().success();
    let out = t.path().join("out");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&out).assert().success();
    for i in 0..methods.len() {
        assert_eq!(fs::read(out.join(format!("m{i}/payload.txt"))).unwrap(), payload, "{:?}", methods[i]);
        assert_eq!(fs::read(out.join(format!("m{i}/small.txt"))).unwrap(), small);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(out.join(format!("m{i}/small.txt"))).unwrap().permissions().mode() & 0o777, 0o640);
        }
    }
    assert!(out.join("emptydir").is_dir());
    #[cfg(unix)]
    assert_eq!(fs::read_link(out.join("m0/link")).unwrap().to_str(), Some("payload.txt"));
    let listing = String::from_utf8(parch().args(["l", "-v"]).arg(&zip).output().unwrap().stdout).unwrap();
    for label in ["Stored", "Defl:N", "BZip2", "Zstd", "XZ", "PPMd"] {
        assert!(listing.contains(label), "{label} missing in\n{listing}");
    }
}

#[test]
fn encrypted_entries_are_reported_not_extracted() {
    let t = tmp();
    let zip = t.path().join("enc.zip");
    {
        let mut w = zip::ZipWriter::new(fs::File::create(&zip).unwrap());
        w.start_file("plain.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"plain").unwrap();
        w.start_file("secret.txt", SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "pw")).unwrap();
        w.write_all(b"secret").unwrap();
        w.finish().unwrap();
    }
    let listing = String::from_utf8(parch().arg("l").arg(&zip).output().unwrap().stdout).unwrap();
    assert!(listing.contains("secret.txt (encrypted)"), "{listing}");
    let out = t.path().join("out");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&out).assert().code(2);
    assert_eq!(fs::read(out.join("plain.txt")).unwrap(), b"plain");
    assert!(!out.join("secret.txt").exists());
}

#[test]
fn legacy_charset_names() {
    let t = tmp();
    write(&t.path().join("src/Документ.txt"), b"cyrillic");
    let zip = t.path().join("cp.zip");
    parch().args(["c", "-q", "--charset", "cp866"]).arg(&zip).arg("src").current_dir(t.path()).assert().success();
    // The Unicode Path field lets every reader recover the UTF-8 name.
    let names: Vec<String> = zip_crate_read(&zip).into_iter().map(|e| e.0).collect();
    assert!(names.contains(&"src/Документ.txt".to_string()), "{names:?}");
    let out = t.path().join("out");
    parch().args(["x", "-q"]).arg(&zip).arg("-C").arg(&out).assert().success();
    assert_eq!(fs::read(out.join("src/Документ.txt")).unwrap(), b"cyrillic");

    // Without the Unicode Path field the reader needs the charset.
    let raw = t.path().join("raw.zip");
    parch().args(["c", "-q", "--charset", "cp866", "-X"]).arg(&raw).arg("src").current_dir(t.path()).assert().success();
    let bytes = fs::read(&raw).unwrap();
    let up = 0x7075u16.to_le_bytes();
    assert!(bytes.windows(2).any(|w| w == up), "unicode path field expected");
    let listing = String::from_utf8(parch().args(["l", "-O", "cp866"]).arg(&raw).output().unwrap().stdout).unwrap();
    assert!(listing.contains("src/Документ.txt"), "{listing}");
}

#[cfg(unix)]
fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

#[cfg(unix)]
#[test]
fn system_tools_accept_parch_archives() {
    let t = tmp();
    let zip = make_archive(&t, &[]);
    let zip64 = t.path().join("z64.zip");
    parch().args(["c", "-q", "--zip64", "always"]).arg(&zip64).arg("src").args(BIG).current_dir(t.path()).assert().success();
    for z in [&zip, &zip64] {
        if tool("unzip").is_some() {
            assert!(run(Command::new("unzip").arg("-tq").arg(z)), "unzip -t {}", z.display());
            let out = t.path().join(format!("unzip-{}", z.file_name().unwrap().to_str().unwrap()));
            fs::create_dir_all(&out).unwrap();
            assert!(run(Command::new("unzip").arg("-q").arg(z).current_dir(&out)));
            assert_same_tree(&t.path().join("src"), &out.join("src"));
        }
        if tool("bsdtar").is_some() {
            assert!(run(Command::new("bsdtar").arg("-tf").arg(z)), "bsdtar -tf");
        }
        if tool("python3").is_some() {
            assert!(run(Command::new("python3").args(["-I", "-m", "zipfile", "-t"]).arg(z)), "python zipfile -t");
        }
        if tool("ditto").is_some() {
            let out = t.path().join(format!("ditto-{}", z.file_name().unwrap().to_str().unwrap()));
            assert!(run(Command::new("ditto").args(["-x", "-k"]).arg(z).arg(&out)), "ditto -x -k");
            assert_eq!(fs::read(out.join("src/sub/text.txt")).unwrap(), fs::read(t.path().join("src/sub/text.txt")).unwrap());
        }
    }
}

#[cfg(unix)]
#[test]
fn parch_reads_system_tool_archives() {
    let t = tmp();
    sample_tree(&t.path().join("src"), true);
    if tool("zip").is_some() {
        let z = t.path().join("infozip.zip");
        assert!(run(Command::new("zip").args(["-qry"]).arg(&z).arg("src").current_dir(t.path())));
        parch().args(["t", "-q"]).arg(&z).assert().success();
        let out = t.path().join("out-infozip");
        parch().args(["x", "-q"]).arg(&z).arg("-C").arg(&out).assert().success();
        assert_same_tree(&t.path().join("src"), &out.join("src"));
        // Streamed Info-ZIP archives use data descriptors.
        let streamed = t.path().join("stream.zip");
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("cat src/sub/text.txt | zip -q - - > {}", streamed.display()))
            .current_dir(t.path())
            .status()
            .unwrap();
        assert!(status.success());
        let data = parch().args(["x", "-p"]).arg(&streamed).output().unwrap().stdout;
        assert_eq!(data, fs::read(t.path().join("src/sub/text.txt")).unwrap());
    }
    if tool("python3").is_some() {
        let z = t.path().join("py.zip");
        let script = "import zipfile,sys\nwith zipfile.ZipFile(sys.argv[1],'w') as z:\n for m in (zipfile.ZIP_STORED,zipfile.ZIP_DEFLATED,zipfile.ZIP_BZIP2,zipfile.ZIP_LZMA):\n  z.write('src/sub/text.txt',f'm{m}/text.txt',compress_type=m)\n";
        assert!(run(Command::new("python3").args(["-I", "-c", script]).arg(&z).current_dir(t.path())));
        let out = t.path().join("out-py");
        parch().args(["x", "-q"]).arg(&z).arg("-C").arg(&out).assert().success();
        for m in [0, 8, 12, 14] {
            assert_eq!(fs::read(out.join(format!("m{m}/text.txt"))).unwrap(), fs::read(t.path().join("src/sub/text.txt")).unwrap());
        }
    }
    if tool("ditto").is_some() {
        let z = t.path().join("ditto.zip");
        assert!(run(Command::new("ditto").args(["-c", "-k", "--norsrc", "--keepParent", "src"]).arg(&z).current_dir(t.path())));
        let out = t.path().join("out-ditto");
        parch().args(["x", "-q"]).arg(&z).arg("-C").arg(&out).assert().success();
        assert_same_tree(&t.path().join("src"), &out.join("src"));
    }
}
