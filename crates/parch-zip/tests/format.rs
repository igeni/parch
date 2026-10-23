use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Command;

use parch_core::codec::CodecId;
use parch_core::entry::{EntryKind, EntryMeta, Time, S_IFREG};
use parch_core::format::{ArchiveFormat, CreateOpts, OpenOpts};
use parch_core::sink::{FileSink, StreamSink};
use parch_zip::ZIP;

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("z").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

fn meta(name: &str, size: u64) -> EntryMeta {
    let mut m = EntryMeta::new(name.as_bytes().to_vec(), EntryKind::File);
    m.size = size;
    m.mode = Some(S_IFREG | 0o644);
    m.mtime = Some(Time::new(1_700_000_000, 0));
    m
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = parch_codecs::get(CodecId::Deflate).unwrap().encoder(6).unwrap();
    let mut out = Vec::new();
    assert!(enc.compress(data, &mut out).unwrap());
    out
}

fn chunked(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
    let mut enc = parch_codecs::get(CodecId::Deflate).unwrap().encoder(6).unwrap();
    let mut parts = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let end = (off + chunk).min(data.len());
        let mut out = Vec::new();
        enc.compress_chunk(&data[off.saturating_sub(32 << 10)..off], &data[off..end], end == data.len(), &mut out).unwrap();
        parts.push(out);
        off = end;
    }
    parts
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| b"0123456789abcdef"[(i * 7 + i / 13) % 16]).collect()
}

fn read_all(path: &Path) -> Vec<(String, Vec<u8>)> {
    let r = ZIP.open(path, &OpenOpts::default()).unwrap();
    let mut out = Vec::new();
    for i in 0..r.entries().len() {
        let mut v = Vec::new();
        r.open_entry(i).unwrap().read_to_end(&mut v).unwrap();
        assert_eq!(v, r.read_entry(i).unwrap());
        out.push((r.entries()[i].name.clone(), v));
    }
    let mut z = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
    assert_eq!(z.len(), out.len());
    for (i, (name, data)) in out.iter().enumerate() {
        let mut f = z.by_index(i).unwrap();
        assert_eq!(f.name().unwrap(), name.as_str());
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        assert_eq!(&v, data);
    }
    out
}

fn unzip_ok(path: &Path) {
    if Command::new("unzip").arg("-v").output().is_ok() {
        let st = Command::new("unzip").arg("-tq").arg(path).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stdout));
    }
}

fn write_sample(path: &Path, stream: bool, opts: &CreateOpts) -> Vec<u8> {
    let big = payload(700_000);
    let file = File::create(path).unwrap();
    let sink: Box<dyn parch_core::sink::Sink> = if stream { Box::new(StreamSink::new(file)) } else { Box::new(FileSink::new(file, false)) };
    let mut w = ZIP.create(sink, opts).unwrap();
    let small = b"small entry contents, small entry contents".to_vec();
    w.add(&meta("small.txt", small.len() as u64), CodecId::Deflate, crc32fast::hash(&small), &deflate(&small)).unwrap();
    w.add(&meta("stored.bin", 3), CodecId::Store, crc32fast::hash(b"abc"), b"abc").unwrap();
    // A chunked entry that is dropped half way.
    w.begin(&meta("aborted.bin", 1000), CodecId::Store).unwrap();
    w.chunk(b"partial").unwrap();
    if !stream {
        w.abort_entry().unwrap();
    } else {
        assert!(w.abort_entry().is_err());
        return big;
    }
    w.begin(&meta("big.txt", big.len() as u64), CodecId::Deflate).unwrap();
    for part in chunked(&big, 100_000) {
        w.chunk(&part).unwrap();
    }
    w.end(crc32fast::hash(&big), big.len() as u64).unwrap();
    w.finish().unwrap();
    big
}

#[test]
fn seekable_output_patches_local_headers() {
    let t = tmp();
    let p = t.path().join("a.zip");
    let big = write_sample(&p, false, &CreateOpts { extra_fields: true, sort_index: true, ..Default::default() });
    let entries = read_all(&p);
    let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["big.txt", "small.txt", "stored.bin"]);
    assert_eq!(entries[0].1, big);
    let bytes = fs::read(&p).unwrap();
    assert!(!bytes.windows(4).any(|w| w == 0x0807_4b50u32.to_le_bytes()), "no data descriptors on seekable output");
    unzip_ok(&p);
}

#[test]
fn stream_output_uses_data_descriptors() {
    let t = tmp();
    let p = t.path().join("s.zip");
    let big = payload(500_000);
    {
        let mut w = ZIP.create(Box::new(StreamSink::new(File::create(&p).unwrap())), &CreateOpts::default()).unwrap();
        w.begin(&meta("big.txt", big.len() as u64), CodecId::Deflate).unwrap();
        for part in chunked(&big, 64 << 10) {
            w.chunk(&part).unwrap();
        }
        w.end(crc32fast::hash(&big), big.len() as u64).unwrap();
        w.add(&meta("after.txt", 2), CodecId::Store, crc32fast::hash(b"ok"), b"ok").unwrap();
        w.finish().unwrap();
    }
    let entries = read_all(&p);
    assert_eq!(entries[0].1, big);
    assert_eq!(entries[1].1, b"ok");
    let bytes = fs::read(&p).unwrap();
    assert!(bytes.windows(4).any(|w| w == 0x0807_4b50u32.to_le_bytes()));
    unzip_ok(&p);
    let st = write_sample(&t.path().join("s2.zip"), true, &CreateOpts::default());
    assert!(!st.is_empty());
}

#[test]
fn forced_zip64_is_readable_everywhere() {
    let t = tmp();
    let p = t.path().join("z64.zip");
    write_sample(&p, false, &CreateOpts { force_zip64: true, comment: b"zip64 comment".to_vec(), ..Default::default() });
    let bytes = fs::read(&p).unwrap();
    assert!(bytes.windows(4).any(|w| w == 0x0606_4b50u32.to_le_bytes()), "ZIP64 end record");
    assert!(bytes.windows(4).any(|w| w == 0x0706_4b50u32.to_le_bytes()), "ZIP64 locator");
    read_all(&p);
    let r = ZIP.open(&p, &OpenOpts::default()).unwrap();
    assert_eq!(r.comment(), b"zip64 comment");
    unzip_ok(&p);
}

#[test]
fn more_than_65535_entries_switch_to_zip64() {
    let t = tmp();
    let p = t.path().join("many.zip");
    let n = 70_000;
    {
        let mut w = ZIP.create(Box::new(FileSink::new(File::create(&p).unwrap(), false)), &CreateOpts::default()).unwrap();
        for i in 0..n {
            let data = format!("{i}");
            w.add(&meta(&format!("d{}/f{i}", i % 100), data.len() as u64), CodecId::Store, crc32fast::hash(data.as_bytes()), data.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }
    let r = ZIP.open(&p, &OpenOpts::default()).unwrap();
    assert_eq!(r.entries().len(), n);
    assert_eq!(r.read_entry(n - 1).unwrap(), format!("{}", n - 1).as_bytes());
    assert_eq!(zip::ZipArchive::new(File::open(&p).unwrap()).unwrap().len(), n);
    unzip_ok(&p);
}

#[test]
fn raw_copy_keeps_entries_and_rebuilds_offsets() {
    let t = tmp();
    let src = t.path().join("src.zip");
    let big = write_sample(&src, false, &CreateOpts::default());
    let base = ZIP.open(&src, &OpenOpts::default()).unwrap();
    let keep_names = ["big.txt", "stored.bin"];
    let keep: Vec<bool> = base.entries().iter().map(|e| keep_names.contains(&e.name.as_str())).collect();
    let dst = t.path().join("dst.zip");
    let mut w = ZIP.create_from(&*base, &|i| keep[i], Box::new(FileSink::new(File::create(&dst).unwrap(), false)), &CreateOpts { sort_index: true, ..Default::default() }).unwrap();
    w.add(&meta("added.txt", 1), CodecId::Store, crc32fast::hash(b"+"), b"+").unwrap();
    w.finish().unwrap();
    let entries = read_all(&dst);
    let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["stored.bin", "big.txt", "added.txt"]);
    assert_eq!(entries[1].1, big);
    unzip_ok(&dst);
}
