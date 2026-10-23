//! The progress bar shows processed bytes (before compression) relative to the whole task.

use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use indicatif::{ProgressDrawTarget, TermLike};
use parch_core::codec::{Caps, Codec, CodecId, DecodeParams, Encoder};
use parch_core::create::{self, CreateConfig, Plan};
use parch_core::entry::EntryMeta;
use parch_core::format::{ArchiveWriter, Summary};
use parch_core::report::Reporter;
use parch_core::scan::ScanOpts;
use parch_core::Result;

#[derive(Debug)]
struct NullTerm;

impl TermLike for NullTerm {
    fn width(&self) -> u16 {
        80
    }
    fn move_cursor_up(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn move_cursor_down(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn move_cursor_right(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn move_cursor_left(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_line(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn write_str(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn clear_line(&self) -> io::Result<()> {
        Ok(())
    }
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

struct StoreOnly;

impl Codec for StoreOnly {
    fn id(&self) -> CodecId {
        CodecId::Store
    }
    fn caps(&self) -> Caps {
        Caps { encode: true, parallel_chunks: true }
    }
    fn encoder(&self, _: u32) -> Option<Box<dyn Encoder>> {
        None
    }
    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        Ok(input)
    }
}

static STORE: StoreOnly = StoreOnly;

/// Records the bar state every time data is written.
struct Probe {
    bar: indicatif::ProgressBar,
    seen: Arc<Mutex<Vec<(Option<u64>, u64)>>>,
}

impl Probe {
    fn record(&self) {
        self.seen.lock().unwrap().push((self.bar.length(), self.bar.position()));
    }
}

impl ArchiveWriter for Probe {
    fn add(&mut self, _: &EntryMeta, _: CodecId, _: u32, _: &[u8]) -> Result<()> {
        self.record();
        Ok(())
    }
    fn begin(&mut self, _: &EntryMeta, _: CodecId) -> Result<()> {
        Ok(())
    }
    fn chunk(&mut self, _: &[u8]) -> Result<()> {
        self.record();
        Ok(())
    }
    fn end(&mut self, _: u32, _: u64) -> Result<()> {
        Ok(())
    }
    fn abort_entry(&mut self) -> Result<()> {
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Summary> {
        Ok(Summary::default())
    }
}

fn tree(root: &Path) -> u64 {
    let mut total = 0;
    for i in 0..400u64 {
        let size = (i * 7919) % 50_000;
        let dir = root.join(format!("d{}", i % 13));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("f{i}.txt")), vec![b'x'; size as usize]).unwrap();
        total += size;
    }
    fs::write(root.join("big.bin"), vec![7u8; 3 << 20]).unwrap();
    total + (3 << 20)
}

fn config(sort: bool) -> CreateConfig {
    CreateConfig {
        threads: 4,
        memory: 16 << 20,
        codec: &STORE,
        level: 6,
        chunk_size: 64 << 10,
        big_file: 1 << 20,
        store_ext: Default::default(),
        sort,
        stream_output: false,
        normalize: None,
        scan: ScanOpts { dir_entries: true, ..Default::default() },
        base: None,
        dedupe: false,
    }
}

fn run(sort: bool) {
    let t = tempfile::Builder::new().prefix("p").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let total = tree(t.path());
    let report = Reporter::with_draw_target(false, 0, ProgressDrawTarget::term_like(Box::new(NullTerm)));
    assert!(report.progress_visible());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe { bar: report.bar().clone(), seen: seen.clone() };
    let factory: create::WriterFactory = Box::new(move || Ok(Box::new(probe) as Box<dyn ArchiveWriter>));
    let plan = if sort { Plan::Collect(Box::new(move |list| Ok((list, factory)))) } else { Plan::Stream(factory) };
    let inputs: Vec<PathBuf> = vec![t.path().to_path_buf()];
    create::run(&config(sort), &inputs, plan, &report, &AtomicBool::new(false)).unwrap();

    let seen = seen.lock().unwrap();
    assert!(seen.len() > 400, "writes recorded: {}", seen.len());
    assert!(seen.iter().all(|(len, _)| *len == Some(total)), "task size must be known before any data is written");
    assert!(seen.windows(2).all(|w| w[0].1 <= w[1].1), "progress must not go back");
    assert_eq!(report.bar().length(), Some(total));
    assert_eq!(report.bar().position(), total);
}

#[test]
fn streaming_pipeline_knows_the_task_size_up_front() {
    run(false);
}

#[test]
fn sorted_pipeline_knows_the_task_size_up_front() {
    run(true);
}
