//! Parallel archive creation pipeline.
//!
//! ```text
//! inputs ─► scan (N workers) ─► dispatcher ─► compress (N workers) ─► writer thread ─► sink
//! ```
//!
//! One pool of `threads` workers both reads directories and compresses data; compression has
//! priority and scanning pauses when too many found entries wait for dispatch. Only the dispatcher
//! blocks on the memory budgets, so the pipeline cannot deadlock. Small files are compressed in
//! batches, medium files one per job, big files in parallel chunks that form one stream.
//!
//! By default the writer stores entries in completion order; chunks of big files are written in
//! order and big files in dispatch order. With `sort` everything is written in dispatch order,
//! which makes the output independent of the thread count.
//!
//! Progress is the uncompressed size of processed files relative to the size of the whole task.
//! When the progress bar is shown, the task size is first counted by a quick parallel pass over
//! the inputs (top priority in the pool, no entries kept in memory).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{select, unbounded, Receiver, RecvTimeoutError, Sender, TryRecvError};

use crate::budget::Budget;
use crate::codec::{Codec, CodecId, Encoder};
use crate::entry::{EntryKind, EntryMeta, Time, S_IFDIR, S_IFLNK, S_IFREG};
use crate::format::{ArchiveWriter, Summary};
use crate::fsmeta;
use crate::report::Reporter;
use crate::scan::{self, DirJob, Found, ScanOpts, ScanSink};
use crate::util;
use crate::{Error, Result};

const SMALL_FILE: u64 = 256 << 10;
const BATCH_FILES: usize = 128;
const BATCH_BYTES: u64 = 4 << 20;
const DICT_SIZE: usize = 32 << 10;
const PROBE_SIZE: usize = 64 << 10;
const PAUSE_SCAN_AT: usize = 100_000;

/// Metadata normalization for reproducible archives.
#[derive(Clone, Copy, Debug, Default)]
pub struct Normalize {
    /// Upper bound for modification times (`SOURCE_DATE_EPOCH`).
    pub mtime_clamp: Option<Time>,
}

pub struct CreateConfig {
    pub threads: usize,
    pub memory: usize,
    /// Store or a codec with an encoder.
    pub codec: &'static dyn Codec,
    pub level: u32,
    pub chunk_size: usize,
    /// Files larger than this are compressed in parallel chunks.
    pub big_file: u64,
    /// Lowercase extensions stored without compression.
    pub store_ext: HashSet<String>,
    /// Write entries in dispatch order (reproducible layout).
    pub sort: bool,
    /// The output cannot be patched (pipe): big files are never stored uncompressed.
    pub stream_output: bool,
    pub normalize: Option<Normalize>,
    pub scan: ScanOpts,
    /// Directory the input paths are relative to.
    pub base: Option<PathBuf>,
    /// Inputs overlap: drop entries whose names were already seen.
    pub dedupe: bool,
}

pub type WriterFactory = Box<dyn FnOnce() -> Result<Box<dyn ArchiveWriter>> + Send>;
pub type Finalize<'a> = Box<dyn FnOnce(Vec<Found>) -> Result<(Vec<Found>, WriterFactory)> + 'a>;

pub enum Plan<'a> {
    /// Entries flow to the writer while scanning continues.
    Stream(WriterFactory),
    /// All entries are collected first; `finalize` filters/orders them and builds the writer.
    Collect(Finalize<'a>),
}

/// Sort key used for reproducible order: the archive name, directories with a trailing `/`.
pub fn sort_key(meta: &EntryMeta) -> Vec<u8> {
    let mut k = meta.name.clone();
    if meta.kind == EntryKind::Dir {
        k.push(b'/');
    }
    k
}

struct BigFile {
    meta: EntryMeta,
    file: File,
    codec: CodecId,
}

enum Work {
    Batch { seq: u64, files: Vec<Found>, cost: usize },
    Chunk { seq: u64, big: u64, idx: u32, total: u32, file: Arc<BigFile>, offset: u64, len: usize, cost: usize },
}

struct DoneItem {
    meta: EntryMeta,
    codec: CodecId,
    crc: u32,
    range: Range<usize>,
    error: Option<Error>,
}

enum Done {
    Items { seq: u64, items: Vec<DoneItem>, buf: Vec<u8>, cost: usize },
    Chunk { seq: u64, big: u64, idx: u32, total: u32, file: Arc<BigFile>, data: Vec<u8>, crc: crc32fast::Hasher, len: u64, cost: usize, error: Option<Error> },
}

impl Done {
    fn seq(&self) -> u64 {
        match self {
            Done::Items { seq, .. } | Done::Chunk { seq, .. } => *seq,
        }
    }
}

enum FoundMsg {
    Entry(Box<Found>),
    ScanDone,
}

struct Shared {
    work_rx: Receiver<Work>,
    scan_rx: Receiver<DirJob>,
    scan_tx: Sender<DirJob>,
    found_tx: Sender<FoundMsg>,
    pending_found: AtomicUsize,
    outstanding_dirs: AtomicUsize,
    pause_limit: usize,
    general: Budget,
    chunks: Budget,
    /// Task size count for the progress bar.
    count_rx: Receiver<DirJob>,
    count_tx: Sender<DirJob>,
    count_outstanding: AtomicUsize,
    count_total: AtomicU64,
    count_seen: Option<Mutex<HashSet<Vec<u8>>>>,
}

/// Runs the pipeline over `inputs`.
pub fn run(cfg: &CreateConfig, inputs: &[PathBuf], plan: Plan<'_>, report: &Reporter, cancel: &AtomicBool) -> Result<Summary> {
    let collect = matches!(plan, Plan::Collect(_));
    let (work_tx, work_rx) = unbounded();
    let (scan_tx, scan_rx) = unbounded();
    let (found_tx, found_rx) = unbounded();
    let (done_tx, done_rx) = unbounded();
    let (count_tx, count_rx) = unbounded();
    let counting = !collect && report.progress_visible();
    let chunk_cost = 2 * cfg.chunk_size + DICT_SIZE;
    let chunk_budget = (cfg.memory / 4).max(chunk_cost * (2 * cfg.threads + 2));
    let sh = Shared {
        work_rx,
        scan_rx,
        scan_tx,
        found_tx,
        pending_found: AtomicUsize::new(0),
        outstanding_dirs: AtomicUsize::new(1),
        pause_limit: if collect { usize::MAX } else { PAUSE_SCAN_AT },
        general: Budget::new(cfg.memory - cfg.memory / 4),
        chunks: Budget::new(chunk_budget),
        count_rx,
        count_tx,
        count_outstanding: AtomicUsize::new(1),
        count_total: AtomicU64::new(0),
        count_seen: (counting && cfg.dedupe).then(|| Mutex::new(HashSet::new())),
    };
    // The counting pass repeats the scan; its warnings would duplicate the real ones.
    let quiet = Reporter::new(true, 0, false);

    let (sh, quiet) = (&sh, &quiet);
    thread::scope(|s| {
        let workers: Vec<_> = (0..cfg.threads)
            .map(|_| {
                let done_tx = done_tx.clone();
                s.spawn(move || worker(sh, cfg, report, quiet, cancel, done_tx))
            })
            .collect();

        if counting {
            report.start_counting();
            let mut sink = CountSink { sh };
            for input in inputs {
                scan::scan_root(input, cfg.base.as_deref(), &cfg.scan, &mut sink, quiet);
            }
            if sh.count_outstanding.fetch_sub(1, Ordering::AcqRel) == 1 {
                report.set_total(sh.count_total.load(Ordering::Relaxed));
            }
            // Nothing is processed before the task size is known.
            while sh.count_outstanding.load(Ordering::Acquire) != 0 && !cancel.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
        }

        let mut roots = RootSink { sh };
        for input in inputs {
            scan::scan_root(input, cfg.base.as_deref(), &cfg.scan, &mut roots, report);
        }
        if sh.outstanding_dirs.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = sh.found_tx.send(FoundMsg::ScanDone);
        }

        let mut disp = Dispatcher::new(cfg, sh, work_tx, done_tx, report, cancel);
        let result = match plan {
            Plan::Stream(factory) => {
                let writer = s.spawn(move || writer_thread(factory, done_rx, sh, cfg.sort, report, cancel));
                disp.stream(&found_rx);
                drop(disp);
                Ok(writer)
            }
            Plan::Collect(finalize) => {
                let all = collect_all(&found_rx, sh, cancel);
                match all.and_then(finalize) {
                    Ok((list, factory)) => {
                        if report.progress_visible() {
                            report.set_total(task_size(&list, cfg.dedupe));
                        }
                        let writer = s.spawn(move || writer_thread(factory, done_rx, sh, cfg.sort, report, cancel));
                        for f in list {
                            if !disp.handle(f) {
                                break;
                            }
                        }
                        disp.flush();
                        drop(disp);
                        Ok(writer)
                    }
                    Err(e) => {
                        cancel.store(true, Ordering::Relaxed);
                        drop(disp);
                        Err(e)
                    }
                }
            }
        };
        for w in workers {
            let _ = w.join();
        }
        let summary = match result {
            Ok(writer) => writer.join().unwrap_or_else(|_| Err(Error::msg("writer thread panicked"))),
            Err(e) => Err(e),
        };
        if cancel.load(Ordering::Relaxed) && summary.is_ok() {
            return Err(Error::Cancelled);
        }
        summary
    })
}

fn collect_all(found_rx: &Receiver<FoundMsg>, sh: &Shared, cancel: &AtomicBool) -> Result<Vec<Found>> {
    let mut all = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        match found_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(FoundMsg::Entry(f)) => {
                sh.pending_found.fetch_sub(1, Ordering::Relaxed);
                all.push(*f);
            }
            Ok(FoundMsg::ScanDone) => return Ok(all),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(all),
        }
    }
}

/// Total uncompressed size of the files in `list`.
fn task_size(list: &[Found], dedupe: bool) -> u64 {
    let mut seen = HashSet::new();
    list.iter()
        .filter(|f| f.meta.kind == EntryKind::File && (!dedupe || seen.insert(sort_key(&f.meta))))
        .map(|f| f.meta.size)
        .sum()
}

/// Sums file sizes for the progress bar; directories become counting jobs.
struct CountSink<'a> {
    sh: &'a Shared,
}

impl ScanSink for CountSink<'_> {
    fn found(&mut self, f: Found) {
        if f.meta.kind != EntryKind::File {
            return;
        }
        if let Some(seen) = &self.sh.count_seen {
            if !seen.lock().unwrap().insert(f.meta.name) {
                return;
            }
        }
        self.sh.count_total.fetch_add(f.meta.size, Ordering::Relaxed);
    }

    fn dir(&mut self, d: DirJob) {
        self.sh.count_outstanding.fetch_add(1, Ordering::AcqRel);
        let _ = self.sh.count_tx.send(d);
    }
}

struct RootSink<'a> {
    sh: &'a Shared,
}

impl ScanSink for RootSink<'_> {
    fn found(&mut self, f: Found) {
        self.sh.pending_found.fetch_add(1, Ordering::Relaxed);
        let _ = self.sh.found_tx.send(FoundMsg::Entry(Box::new(f)));
    }

    fn dir(&mut self, d: DirJob) {
        self.sh.outstanding_dirs.fetch_add(1, Ordering::AcqRel);
        let _ = self.sh.scan_tx.send(d);
    }
}

fn normalize(meta: &mut EntryMeta, n: Normalize) {
    let exec = meta.mode.is_some_and(|m| m & 0o111 != 0);
    meta.mode = Some(match meta.kind {
        EntryKind::Dir => S_IFDIR | 0o755,
        EntryKind::Symlink => S_IFLNK | 0o777,
        EntryKind::File => S_IFREG | if exec { 0o755 } else { 0o644 },
    });
    meta.uid = None;
    meta.gid = None;
    meta.dos_attrs = None;
    meta.atime = None;
    meta.btime = None;
    if let (Some(clamp), Some(m)) = (n.mtime_clamp, meta.mtime) {
        if m > clamp {
            meta.mtime = Some(clamp);
        }
    }
}

struct Dispatcher<'a> {
    cfg: &'a CreateConfig,
    sh: &'a Shared,
    work_tx: Sender<Work>,
    done_tx: Sender<Done>,
    report: &'a Reporter,
    cancel: &'a AtomicBool,
    seq: u64,
    next_big: u64,
    batch: Vec<Found>,
    batch_bytes: u64,
    probe: Option<Box<dyn Encoder>>,
    seen: HashSet<Vec<u8>>,
}

impl<'a> Dispatcher<'a> {
    fn new(cfg: &'a CreateConfig, sh: &'a Shared, work_tx: Sender<Work>, done_tx: Sender<Done>, report: &'a Reporter, cancel: &'a AtomicBool) -> Self {
        Self {
            cfg,
            sh,
            work_tx,
            done_tx,
            report,
            cancel,
            seq: 0,
            next_big: 0,
            batch: Vec::new(),
            batch_bytes: 0,
            probe: None,
            seen: HashSet::new(),
        }
    }

    fn stream(&mut self, found_rx: &Receiver<FoundMsg>) {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return;
            }
            let msg = match found_rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => {
                    if !self.flush() {
                        return;
                    }
                    match found_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(m) => m,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                Err(TryRecvError::Disconnected) => break,
            };
            match msg {
                FoundMsg::Entry(f) => {
                    self.sh.pending_found.fetch_sub(1, Ordering::Relaxed);
                    if !self.handle(*f) {
                        return;
                    }
                }
                FoundMsg::ScanDone => break,
            }
        }
        self.flush();
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq - 1
    }

    /// Returns false when cancelled.
    fn handle(&mut self, mut f: Found) -> bool {
        if self.cfg.dedupe && !self.seen.insert(sort_key(&f.meta)) {
            return true;
        }
        if let Some(n) = self.cfg.normalize {
            normalize(&mut f.meta, n);
        }
        let size = f.meta.size;
        match f.meta.kind {
            EntryKind::Dir => self.direct(f.meta, Vec::new()),
            EntryKind::Symlink => {
                let target = f.meta.link_target.clone().unwrap_or_default();
                self.direct(f.meta, target)
            }
            EntryKind::File => {
                let chunked = self.cfg.codec.id() == CodecId::Store || self.cfg.codec.caps().parallel_chunks;
                if size > self.cfg.big_file && chunked {
                    return self.flush() && self.big(f);
                }
                if size >= SMALL_FILE && !self.flush() {
                    return false;
                }
                self.batch_bytes += size;
                self.batch.push(f);
                if size >= SMALL_FILE || self.batch.len() >= BATCH_FILES || self.batch_bytes >= BATCH_BYTES {
                    return self.flush();
                }
                true
            }
        }
    }

    fn direct(&mut self, meta: EntryMeta, data: Vec<u8>) -> bool {
        if !self.flush() {
            return false;
        }
        let crc = crc32fast::hash(&data);
        let range = 0..data.len();
        let seq = self.next_seq();
        let item = DoneItem { meta, codec: CodecId::Store, crc, range, error: None };
        let _ = self.done_tx.send(Done::Items { seq, items: vec![item], buf: data, cost: 0 });
        true
    }

    fn flush(&mut self) -> bool {
        if self.batch.is_empty() {
            return true;
        }
        let files = std::mem::take(&mut self.batch);
        let want = (2 * self.batch_bytes as usize).saturating_add(files.len() * 512);
        self.batch_bytes = 0;
        let Some(cost) = self.sh.general.acquire(want, self.cancel) else { return false };
        let seq = self.next_seq();
        let _ = self.work_tx.send(Work::Batch { seq, files, cost });
        true
    }

    fn big(&mut self, f: Found) -> bool {
        let file = match File::open(&f.path) {
            Ok(file) => file,
            Err(e) => {
                self.report.warn(format!("{}: {e}", f.path.display()));
                return true;
            }
        };
        let size = f.meta.size;
        let codec = self.big_codec(&f.meta, &file);
        let chunk = self.cfg.chunk_size as u64;
        let total = size.div_ceil(chunk) as u32;
        let big = Arc::new(BigFile { meta: f.meta, file, codec });
        let id = self.next_big;
        self.next_big += 1;
        for idx in 0..total {
            let offset = idx as u64 * chunk;
            let len = chunk.min(size - offset) as usize;
            let Some(cost) = self.sh.chunks.acquire(2 * len + DICT_SIZE, self.cancel) else { return false };
            let seq = self.next_seq();
            let _ = self.work_tx.send(Work::Chunk { seq, big: id, idx, total, file: big.clone(), offset, len, cost });
        }
        true
    }

    fn big_codec(&mut self, meta: &EntryMeta, file: &File) -> CodecId {
        let codec = self.cfg.codec.id();
        if codec == CodecId::Store {
            return CodecId::Store;
        }
        if self.cfg.stream_output {
            return codec;
        }
        if util::extension(&meta.name).is_some_and(|e| self.cfg.store_ext.contains(&e)) {
            return CodecId::Store;
        }
        let mut sample = vec![0u8; PROBE_SIZE.min(meta.size as usize)];
        if fsmeta::read_exact_at(file, &mut sample, 0).is_err() {
            return codec;
        }
        let probe = self.probe.get_or_insert_with(|| self.cfg.codec.encoder(1).expect("encoder"));
        let mut out = Vec::new();
        match probe.compress(&sample, &mut out) {
            Ok(true) if out.len() * 100 < sample.len() * 97 => codec,
            Ok(_) => CodecId::Store,
            Err(_) => codec,
        }
    }
}

fn worker(sh: &Shared, cfg: &CreateConfig, report: &Reporter, quiet: &Reporter, cancel: &AtomicBool, done_tx: Sender<Done>) {
    let mut w = Worker { sh, cfg, report, quiet, done_tx, enc: cfg.codec.encoder(cfg.level), input: Vec::new() };
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(dir) = sh.count_rx.try_recv() {
            w.count(dir);
            continue;
        }
        match sh.work_rx.try_recv() {
            Ok(job) => {
                w.run(job);
                continue;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        if sh.pending_found.load(Ordering::Relaxed) >= sh.pause_limit {
            match sh.work_rx.recv_timeout(Duration::from_millis(5)) {
                Ok(job) => w.run(job),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            continue;
        }
        select! {
            recv(sh.count_rx) -> dir => if let Ok(dir) = dir {
                w.count(dir);
            },
            recv(sh.work_rx) -> job => match job {
                Ok(job) => w.run(job),
                Err(_) => break,
            },
            recv(sh.scan_rx) -> dir => if let Ok(dir) = dir {
                w.scan(dir);
            },
        }
    }
}

struct Worker<'a> {
    sh: &'a Shared,
    cfg: &'a CreateConfig,
    report: &'a Reporter,
    quiet: &'a Reporter,
    done_tx: Sender<Done>,
    enc: Option<Box<dyn Encoder>>,
    input: Vec<u8>,
}

impl ScanSink for Worker<'_> {
    fn found(&mut self, f: Found) {
        self.sh.pending_found.fetch_add(1, Ordering::Relaxed);
        let _ = self.sh.found_tx.send(FoundMsg::Entry(Box::new(f)));
    }

    fn dir(&mut self, d: DirJob) {
        self.sh.outstanding_dirs.fetch_add(1, Ordering::AcqRel);
        let _ = self.sh.scan_tx.send(d);
    }
}

impl Worker<'_> {
    fn count(&mut self, dir: DirJob) {
        let mut sink = CountSink { sh: self.sh };
        scan::scan_dir(&dir, &self.cfg.scan, &mut sink, self.quiet);
        if self.sh.count_outstanding.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.report.set_total(self.sh.count_total.load(Ordering::Relaxed));
        }
    }

    fn scan(&mut self, dir: DirJob) {
        let (cfg, report) = (self.cfg, self.report);
        scan::scan_dir(&dir, &cfg.scan, self, report);
        if self.sh.outstanding_dirs.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.sh.found_tx.send(FoundMsg::ScanDone);
        }
    }

    fn run(&mut self, job: Work) {
        let done = match job {
            Work::Batch { seq, files, cost } => self.batch(seq, files, cost),
            Work::Chunk { seq, big, idx, total, file, offset, len, cost } => self.chunk(seq, big, idx, total, file, offset, len, cost),
        };
        let _ = self.done_tx.send(done);
    }

    fn batch(&mut self, seq: u64, files: Vec<Found>, cost: usize) -> Done {
        let est: u64 = files.iter().map(|f| f.meta.size).sum();
        let mut buf = Vec::with_capacity(est as usize / 2 + 64);
        let mut items = Vec::with_capacity(files.len());
        for f in files {
            let start = buf.len();
            match self.compress_file(&f, &mut buf) {
                Ok((codec, crc, size)) => {
                    let mut meta = f.meta;
                    meta.size = size;
                    items.push(DoneItem { meta, codec, crc, range: start..buf.len(), error: None });
                }
                Err(e) => {
                    buf.truncate(start);
                    let path = f.path.display().to_string();
                    items.push(DoneItem { meta: f.meta, codec: CodecId::Store, crc: 0, range: start..start, error: Some(Error::msg(format!("{path}: {e}"))) });
                }
            }
        }
        Done::Items { seq, items, buf, cost }
    }

    fn compress_file(&mut self, f: &Found, out: &mut Vec<u8>) -> Result<(CodecId, u32, u64)> {
        read_file(&f.path, f.meta.size as usize, &mut self.input)?;
        let crc = crc32fast::hash(&self.input);
        let size = self.input.len() as u64;
        let store = util::extension(&f.meta.name).is_some_and(|e| self.cfg.store_ext.contains(&e));
        let mut codec = CodecId::Store;
        if let (false, Some(enc)) = (store, self.enc.as_mut()) {
            if enc.compress(&self.input, out)? {
                codec = self.cfg.codec.id();
            }
        }
        if codec == CodecId::Store {
            out.extend_from_slice(&self.input);
        }
        if self.input.capacity() > (16 << 20) {
            self.input = Vec::new();
        }
        Ok((codec, crc, size))
    }

    #[allow(clippy::too_many_arguments)]
    fn chunk(&mut self, seq: u64, big: u64, idx: u32, total: u32, file: Arc<BigFile>, offset: u64, len: usize, cost: usize) -> Done {
        let last = idx + 1 == total;
        let dict_len = if file.codec == CodecId::Store { 0 } else { (DICT_SIZE as u64).min(offset) as usize };
        let mut crc = crc32fast::Hasher::new();
        let result = (|| -> Result<Vec<u8>> {
            let mut buf = vec![0u8; dict_len + len];
            fsmeta::read_exact_at(&file.file, &mut buf, offset - dict_len as u64).map_err(|e| {
                if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::msg("file shrank while archiving") } else { e.into() }
            })?;
            if last {
                let mut one = [0u8; 1];
                if let Ok(1) = fsmeta::read_at(&file.file, &mut one, offset + len as u64) {
                    self.report.warn(format!("{}: file grew while archiving, extra data ignored", file.meta.name_lossy()));
                }
            }
            crc.update(&buf[dict_len..]);
            if file.codec == CodecId::Store {
                return Ok(buf);
            }
            let enc = self.enc.as_mut().ok_or_else(|| Error::msg("no encoder"))?;
            let mut out = Vec::with_capacity(len / 2 + 64);
            enc.compress_chunk(&buf[..dict_len], &buf[dict_len..], last, &mut out)?;
            Ok(out)
        })();
        let (data, error) = match result {
            Ok(d) => (d, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Done::Chunk { seq, big, idx, total, file, data, crc, len: len as u64, cost, error }
    }
}

/// Reads a whole file whose expected size is known, without the extra `fstat`/`lseek` that
/// `read_to_end` performs to size its buffer.
fn read_file(path: &std::path::Path, size: usize, buf: &mut Vec<u8>) -> std::io::Result<()> {
    let mut f = File::open(path)?;
    buf.clear();
    buf.resize(size + 1, 0);
    let mut len = 0;
    loop {
        match f.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => {
                len += n;
                if len == buf.len() {
                    // The file grew since it was scanned: read the rest.
                    f.read_to_end(buf)?;
                    return Ok(());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(len);
    Ok(())
}

struct StreamState {
    big: u64,
    next_idx: u32,
    crc: crc32fast::Hasher,
    size: u64,
    csize: u64,
    failed: bool,
}

struct WriterState<'a> {
    w: Box<dyn ArchiveWriter>,
    sh: &'a Shared,
    report: &'a Reporter,
    ordered: bool,
    next_seq: u64,
    pending: BTreeMap<u64, Done>,
    open: Option<u64>,
    next_big: u64,
    held: VecDeque<Done>,
    chunks: HashMap<u64, BTreeMap<u32, Done>>,
    stream: Option<StreamState>,
}

fn release(sh: &Shared, d: &Done) {
    match d {
        Done::Items { cost, .. } => sh.general.release(*cost),
        Done::Chunk { cost, .. } => sh.chunks.release(*cost),
    }
}

fn writer_thread(factory: WriterFactory, rx: Receiver<Done>, sh: &Shared, ordered: bool, report: &Reporter, cancel: &AtomicBool) -> Result<Summary> {
    let w = match factory() {
        Ok(w) => w,
        Err(e) => {
            cancel.store(true, Ordering::Relaxed);
            for d in rx.iter() {
                release(sh, &d);
            }
            return Err(e);
        }
    };
    let mut st = WriterState {
        w,
        sh,
        report,
        ordered,
        next_seq: 0,
        pending: BTreeMap::new(),
        open: None,
        next_big: 0,
        held: VecDeque::new(),
        chunks: HashMap::new(),
        stream: None,
    };
    let mut failure = None;
    for d in rx.iter() {
        if failure.is_some() {
            release(sh, &d);
            continue;
        }
        if let Err(e) = st.accept(d) {
            cancel.store(true, Ordering::Relaxed);
            failure = Some(e);
        }
    }
    if let Some(e) = failure {
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if !st.pending.is_empty() || !st.held.is_empty() || !st.chunks.is_empty() || st.stream.is_some() {
        return Err(Error::msg("internal error: pipeline finished with pending entries"));
    }
    st.w.finish()
}

impl WriterState<'_> {
    fn accept(&mut self, d: Done) -> Result<()> {
        if self.ordered {
            self.pending.insert(d.seq(), d);
            while let Some(d) = self.pending.remove(&self.next_seq) {
                self.next_seq += 1;
                self.apply(d)?;
            }
            return Ok(());
        }
        match d {
            Done::Items { .. } if self.open.is_none() => {
                self.apply(d)?;
            }
            Done::Items { .. } => self.held.push_back(d),
            Done::Chunk { big, idx, .. } => {
                self.chunks.entry(big).or_default().insert(idx, d);
            }
        }
        self.drive()
    }

    fn drive(&mut self) -> Result<()> {
        loop {
            if self.open.is_none() {
                while let Some(d) = self.held.pop_front() {
                    self.apply(d)?;
                }
                let ready = self.chunks.get(&self.next_big).is_some_and(|m| m.contains_key(&0));
                if !ready {
                    return Ok(());
                }
                self.open = Some(self.next_big);
            }
            let big = self.open.expect("open stream");
            let mut finished = false;
            loop {
                let next = self.stream.as_ref().map_or(0, |s| s.next_idx);
                let Some(d) = self.chunks.get_mut(&big).and_then(|m| m.remove(&next)) else { break };
                if self.apply(d)? {
                    finished = true;
                    break;
                }
            }
            if !finished {
                return Ok(());
            }
            self.chunks.remove(&big);
            self.open = None;
            self.next_big += 1;
        }
    }

    /// Writes one result; returns true when it completed a chunked entry.
    fn apply(&mut self, d: Done) -> Result<bool> {
        match d {
            Done::Items { items, buf, cost, .. } => {
                let res = (|| {
                    for it in items {
                        if it.meta.kind == EntryKind::File {
                            self.report.bar().inc(it.meta.size);
                        }
                        if let Some(e) = it.error {
                            self.report.warn(e);
                            continue;
                        }
                        self.w.add(&it.meta, it.codec, it.crc, &buf[it.range.clone()])?;
                        self.verbose(&it.meta, it.codec, it.range.len() as u64);
                    }
                    Ok(())
                })();
                self.sh.general.release(cost);
                res.map(|_| false)
            }
            Done::Chunk { big, idx, total, file, data, crc, len, cost, error, .. } => {
                self.sh.chunks.release(cost);
                self.report.bar().inc(len);
                if idx == 0 {
                    self.stream = Some(StreamState { big, next_idx: 0, crc: crc32fast::Hasher::new(), size: 0, csize: 0, failed: false });
                }
                let st = self.stream.as_mut().ok_or_else(|| Error::msg("internal error: chunk without stream"))?;
                if st.big != big || st.next_idx != idx {
                    return Err(Error::msg("internal error: chunks out of order"));
                }
                st.next_idx += 1;
                if !st.failed {
                    match error {
                        Some(e) => {
                            st.failed = true;
                            if idx > 0 {
                                self.w.abort_entry()?;
                            }
                            self.report.warn(format!("{}: {e}", file.meta.name_lossy()));
                        }
                        None => {
                            if idx == 0 {
                                self.w.begin(&file.meta, file.codec)?;
                            }
                            self.w.chunk(&data)?;
                            st.crc.combine(&crc);
                            st.size += len;
                            st.csize += data.len() as u64;
                        }
                    }
                }
                if idx + 1 < total {
                    return Ok(false);
                }
                let st = self.stream.take().expect("stream");
                if !st.failed {
                    self.w.end(st.crc.finalize(), st.size)?;
                    let mut meta = file.meta.clone();
                    meta.size = st.size;
                    self.verbose(&meta, file.codec, st.csize);
                }
                Ok(true)
            }
        }
    }

    fn verbose(&self, meta: &EntryMeta, codec: CodecId, csize: u64) {
        if self.report.verbose() > 0 {
            let dir = if meta.kind == EntryKind::Dir { "/" } else { "" };
            let pct = if meta.size == 0 || codec == CodecId::Store { 0 } else { 100 - (csize.min(meta.size) * 100 / meta.size) };
            let how = match codec {
                CodecId::Store => "stored",
                CodecId::Deflate => "deflated",
                c => c.name(),
            };
            self.report.info(format!("  adding: {}{dir} ({how} {pct}%)", meta.name_lossy()));
        }
    }
}
