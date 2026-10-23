use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Local, TimeZone};
use parch_core::codec::{Codec, CodecId};
use parch_core::create::{self, CreateConfig, Normalize, Plan, WriterFactory};
use parch_core::entry::{EntryKind, Time};
use parch_core::extract::{self, ExtractConfig, Overwrite};
use parch_core::filter::Filter;
use parch_core::format::{ArchiveFormat, ArchiveReader, CreateOpts, OpenOpts, Summary};
use parch_core::fsmeta::RestoreOpts;
use parch_core::report::Reporter;
use parch_core::scan::{self, Found, ScanOpts};
use parch_core::sink::{FileSink, Sink, StreamSink};
use tempfile::{NamedTempFile, TempPath};

use crate::args::*;
use crate::registry;

pub fn run(cli: Cli, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    let g = cli.global;
    match cli.cmd {
        Command::Create(a) => create(&g, a, report, cancel),
        Command::Add(a) => add(&g, a, report, cancel),
        Command::Delete(a) => delete(&g, a, report),
        Command::Extract(a) => extract_cmd(&g, a, report, cancel),
        Command::List(a) => list(&g, a),
        Command::Test(a) => test(&g, a, report, cancel),
    }
}

fn is_stdio(p: &Path) -> bool {
    p == Path::new("-")
}

/// Opens the output: stdout, or a temporary file next to the archive (renamed on success).
fn open_output(archive: &Path) -> Result<(Box<dyn Sink>, Option<TempPath>)> {
    if is_stdio(archive) {
        if io::stdout().is_terminal() {
            bail!("refusing to write an archive to a terminal");
        }
        return Ok((Box::new(StreamSink::new(io::stdout())), None));
    }
    let dir = match archive.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut b = tempfile::Builder::new();
    b.prefix(".parch-").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        b.permissions(fs::Permissions::from_mode(0o666));
    }
    let tmp = b.tempfile_in(dir).with_context(|| format!("cannot create a temporary file in {}", dir.display()))?;
    let (file, path) = tmp.into_parts();
    Ok((Box::new(FileSink::new(file, false)), Some(path)))
}

/// Moves the finished temporary file over the archive, keeping the old file's permissions.
fn persist(temp: TempPath, archive: &Path) -> Result<()> {
    if let Ok(md) = fs::metadata(archive) {
        let _ = fs::set_permissions(&temp, md.permissions());
    }
    temp.persist(archive).map_err(|e| anyhow!("cannot write {}: {}", archive.display(), e.error))
}

/// Archive given on the command line; stdin is spooled to a temporary file.
struct Input {
    path: PathBuf,
    _spool: Option<NamedTempFile>,
}

fn open_input(p: &Path) -> Result<Input> {
    if !is_stdio(p) {
        return Ok(Input { path: p.to_path_buf(), _spool: None });
    }
    let mut t = NamedTempFile::new().context("cannot create a temporary file for stdin")?;
    io::copy(&mut io::stdin().lock(), t.as_file_mut()).context("cannot read the archive from stdin")?;
    t.as_file_mut().flush()?;
    Ok(Input { path: t.path().to_path_buf(), _spool: Some(t) })
}

fn open_reader(g: &Global, input: &Input, charset: Option<String>) -> Result<Box<dyn ArchiveReader>> {
    let format = registry::for_open(&input.path, g.format.as_deref())?;
    Ok(format.open(&input.path, &OpenOpts { charset })?)
}

fn codec_and_level(o: &CompressArgs) -> (&'static dyn Codec, u32) {
    let level = o.level.unwrap_or(6);
    let id = match o.method {
        Some(Method::Store) => CodecId::Store,
        Some(Method::Deflate) => CodecId::Deflate,
        None if level == 0 => CodecId::Store,
        None => CodecId::Deflate,
    };
    (parch_codecs::get(id).expect("built-in codec"), level.max(1))
}

fn inputs(o: &CompressArgs, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut v = paths.to_vec();
    if let Some(list) = &o.files_from {
        let data = if is_stdio(list) {
            let mut d = Vec::new();
            io::stdin().lock().read_to_end(&mut d)?;
            d
        } else {
            fs::read(list).with_context(|| format!("cannot read {}", list.display()))?
        };
        v.extend(scan::parse_list(&data, o.null));
    }
    if v.is_empty() {
        bail!("nothing to add: no paths given");
    }
    Ok(v)
}

/// Whether one input contains another (or repeats it), so names may repeat.
fn overlapping(inputs: &[PathBuf]) -> bool {
    if inputs.len() < 2 {
        return false;
    }
    let names: Vec<Vec<u8>> = inputs.iter().map(|p| scan::archive_name(p).0).collect();
    if names.iter().any(Vec::is_empty) {
        return true;
    }
    let mut set = HashSet::new();
    for n in &names {
        if !set.insert(n.as_slice()) {
            return true;
        }
    }
    names.iter().any(|n| n.iter().enumerate().any(|(i, &b)| b == b'/' && set.contains(&n[..i])))
}

fn store_ext(opt: &Option<String>) -> HashSet<String> {
    opt.as_deref()
        .unwrap_or(DEFAULT_STORE_EXT)
        .split(',')
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

fn source_date_epoch() -> Option<Time> {
    std::env::var("SOURCE_DATE_EPOCH").ok()?.trim().parse::<i64>().ok().map(|s| Time::new(s, 0))
}

fn config(g: &Global, o: &CompressArgs, codec: &'static dyn Codec, level: u32, stream: bool, skip: Vec<PathBuf>, dedupe: bool) -> Result<CreateConfig> {
    let mut exclude = o.exclude.clone();
    for f in &o.exclude_from {
        let text = fs::read_to_string(f).with_context(|| format!("cannot read {}", f.display()))?;
        exclude.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from));
    }
    let filter = Filter::new(&o.include, &exclude)?;
    let chunk_size = o.chunk_size.clamp(64 << 10, 256 << 20) as usize;
    Ok(CreateConfig {
        threads: g.threads(),
        memory: (g.memory as usize).max(16 << 20),
        codec,
        level,
        chunk_size,
        big_file: o.big_file,
        store_ext: store_ext(&o.store_ext),
        sort: o.sort || o.reproducible,
        stream_output: stream,
        normalize: o.reproducible.then(|| Normalize { mtime_clamp: source_date_epoch() }),
        scan: ScanOpts { follow_symlinks: o.follow_symlinks, dir_entries: !o.no_dir_entries, filter, skip },
        base: o.directory.clone(),
        dedupe,
    })
}

fn create_opts(o: &CompressArgs, inherited_comment: Option<&[u8]>) -> CreateOpts {
    CreateOpts {
        comment: o.comment.clone().map(String::into_bytes).or_else(|| inherited_comment.map(<[u8]>::to_vec)).unwrap_or_default(),
        force_zip64: o.zip64 == Zip64::Always,
        extra_fields: !(o.no_extra || o.reproducible),
        utc_times: o.reproducible,
        charset: o.charset.clone(),
        sort_index: true,
    }
}

fn skip_list(archive: &Path, temp: &Option<TempPath>) -> Vec<PathBuf> {
    let mut skip = Vec::new();
    if let Some(t) = temp {
        skip.extend(fs::canonicalize(t).ok());
    }
    if !is_stdio(archive) {
        skip.extend(fs::canonicalize(archive).ok());
    }
    skip
}

fn sort_found(list: &mut [Found]) {
    list.sort_by_cached_key(|f| create::sort_key(&f.meta));
}

fn report_summary(report: &Reporter, s: &Summary) {
    report.info(format!("{} entries, {} bytes in, {} bytes written", s.entries, s.bytes_in, s.bytes_out));
}

fn create(g: &Global, a: CreateArgs, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    let format = registry::for_create(&a.archive, g.format.as_deref())?;
    let inputs = inputs(&a.opts, &a.paths)?;
    let (codec, level) = codec_and_level(&a.opts);
    if !format.can_write(codec.id()) {
        bail!("{} archives cannot be written with {}", format.id(), codec.id().name());
    }
    let (sink, temp) = open_output(&a.archive)?;
    let cfg = config(g, &a.opts, codec, level, temp.is_none(), skip_list(&a.archive, &temp), overlapping(&inputs))?;
    let opts = create_opts(&a.opts, None);
    let factory: WriterFactory = Box::new(move || format.create(sink, &opts));
    let plan = if cfg.sort {
        Plan::Collect(Box::new(move |mut list| {
            sort_found(&mut list);
            Ok((list, factory))
        }))
    } else {
        Plan::Stream(factory)
    };
    let summary = create::run(&cfg, &inputs, plan, report, cancel)?;
    if let Some(t) = temp {
        persist(t, &a.archive)?;
    }
    report_summary(report, &summary);
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddMode {
    Add,
    Update,
    Freshen,
}

fn add(g: &Global, a: AddArgs, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    if is_stdio(&a.archive) {
        bail!("cannot update an archive on stdin/stdout");
    }
    if !a.archive.exists() {
        if a.freshen {
            bail!("{}: archive does not exist", a.archive.display());
        }
        return create(g, CreateArgs { archive: a.archive, paths: a.paths, opts: a.opts }, report, cancel);
    }
    let format = registry::for_open(&a.archive, g.format.as_deref())?;
    let base = format.open(&a.archive, &OpenOpts { charset: a.opts.charset.clone() })?;
    let inputs = inputs(&a.opts, &a.paths)?;
    let (codec, level) = codec_and_level(&a.opts);
    if !format.can_write(codec.id()) {
        bail!("{} archives cannot be written with {}", format.id(), codec.id().name());
    }
    let (sink, temp) = open_output(&a.archive)?;
    let cfg = config(g, &a.opts, codec, level, false, skip_list(&a.archive, &temp), overlapping(&inputs))?;
    let opts = create_opts(&a.opts, Some(base.comment()));
    let mode = if a.update {
        AddMode::Update
    } else if a.freshen {
        AddMode::Freshen
    } else {
        AddMode::Add
    };
    let sort = cfg.sort;
    let unchanged = Arc::new(AtomicBool::new(false));
    let unchanged2 = unchanged.clone();
    let plan = Plan::Collect(Box::new(move |found| {
        let entries = base.entries();
        let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            index.entry(e.name.trim_end_matches('/')).or_default().push(i);
        }
        let mut replace = HashSet::new();
        let mut list = Vec::new();
        for f in found {
            let name = f.meta.name_lossy().into_owned();
            match index.get(name.as_str()) {
                Some(ids) => {
                    let last = *ids.last().expect("non-empty");
                    if f.meta.kind == EntryKind::Dir && entries[last].kind == EntryKind::Dir {
                        continue;
                    }
                    let newer = match (f.meta.mtime, entries[last].mtime) {
                        (Some(n), Some(o)) => n.secs > o.secs,
                        _ => true,
                    };
                    if mode == AddMode::Add || newer {
                        replace.extend(ids.iter().copied());
                        list.push(f);
                    }
                }
                None if mode != AddMode::Freshen => list.push(f),
                None => {}
            }
        }
        if sort {
            sort_found(&mut list);
        }
        if list.is_empty() {
            unchanged2.store(true, Ordering::Relaxed);
            let factory: WriterFactory = Box::new(move || format.create(sink, &opts));
            return Ok((list, factory));
        }
        let factory: WriterFactory = Box::new(move || format.create_from(&*base, &|i| !replace.contains(&i), sink, &opts));
        Ok((list, factory))
    }));
    let summary = create::run(&cfg, &inputs, plan, report, cancel)?;
    if unchanged.load(Ordering::Relaxed) {
        report.info(format!("{}: nothing to update", a.archive.display()));
        return Ok(());
    }
    if let Some(t) = temp {
        persist(t, &a.archive)?;
    }
    report_summary(report, &summary);
    Ok(())
}

fn delete(g: &Global, a: DeleteArgs, report: &Reporter) -> Result<()> {
    if is_stdio(&a.archive) {
        bail!("cannot modify an archive on stdin/stdout");
    }
    let format: &dyn ArchiveFormat = registry::for_open(&a.archive, g.format.as_deref())?;
    let base = format.open(&a.archive, &OpenOpts { charset: a.charset.clone() })?;
    let filter = Filter::new(&a.patterns, &[])?;
    let del: HashSet<usize> = base.entries().iter().enumerate().filter(|(_, e)| filter.included(&e.name)).map(|(i, _)| i).collect();
    if del.is_empty() {
        report.warn("no entries matched; archive unchanged");
        return Ok(());
    }
    for (i, e) in base.entries().iter().enumerate() {
        if del.contains(&i) {
            report.info(format!("deleting: {}", e.name));
        }
    }
    let (sink, temp) = open_output(&a.archive)?;
    let opts = CreateOpts { comment: base.comment().to_vec(), sort_index: false, ..Default::default() };
    let w = format.create_from(&*base, &|i| !del.contains(&i), sink, &opts)?;
    let summary = w.finish()?;
    drop(base);
    if let Some(t) = temp {
        persist(t, &a.archive)?;
    }
    report_summary(report, &summary);
    Ok(())
}

fn extract_cmd(g: &Global, a: ExtractArgs, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    let input = open_input(&a.archive)?;
    let reader = open_reader(g, &input, a.charset.clone())?;
    let filter = Filter::new(&a.patterns, &a.exclude)?;
    let selected = extract::select(&*reader, &filter);
    if selected.is_empty() && !a.patterns.is_empty() {
        report.warn("no entries matched");
    }
    if a.to_stdout {
        let out = io::stdout();
        let mut lock = out.lock();
        extract::to_writer(&*reader, &selected, &mut lock, report)?;
        return Ok(());
    }
    let cfg = ExtractConfig {
        threads: g.threads(),
        dest: a.directory,
        overwrite: if a.overwrite {
            Overwrite::Always
        } else if a.update {
            Overwrite::Newer
        } else {
            Overwrite::Never
        },
        strip_components: a.strip_components,
        flat: a.flat,
        restore: RestoreOpts { perms: !a.no_perms, times: !a.no_times, owner: a.same_owner },
        allow_unsafe_symlinks: a.allow_unsafe_symlinks,
    };
    extract::extract(&*reader, &selected, &cfg, report, cancel)?;
    Ok(())
}

fn fmt_time(t: Option<Time>) -> String {
    t.and_then(|t| Local.timestamp_opt(t.secs, 0).single())
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| " ".repeat(16))
}

fn ratio(size: u64, csize: u64) -> i64 {
    if size == 0 { 0 } else { 100 - (csize as i128 * 100 / size as i128) as i64 }
}

fn list(g: &Global, a: ListArgs) -> Result<()> {
    let input = open_input(&a.archive)?;
    let reader = open_reader(g, &input, a.charset.clone())?;
    let filter = Filter::new(&a.patterns, &[])?;
    let selected = extract::select(&*reader, &filter);
    let entries = reader.entries();
    let out = io::stdout();
    let mut w = BufWriter::new(out.lock());
    let res = (|| -> io::Result<()> {
        if !reader.comment().is_empty() {
            writeln!(w, "{}", String::from_utf8_lossy(reader.comment()))?;
        }
        let (mut total, mut ctotal) = (0u64, 0u64);
        let mark = |e: &parch_core::format::EntryInfo| if e.encrypted { " (encrypted)" } else { "" };
        if g.verbose > 0 {
            writeln!(w, " Length   Method    Size  Cmpr    Date    Time   CRC-32   Name")?;
            writeln!(w, "--------  ------  ------- ---- ---------- ----- --------  ----")?;
            for &i in &selected {
                let e = &entries[i];
                total += e.size;
                ctotal += e.compressed_size;
                writeln!(w, "{:>8}  {:<6} {:>8} {:>3}% {} {:08x}  {}{}", e.size, reader.method_label(i), e.compressed_size, ratio(e.size, e.compressed_size), fmt_time(e.mtime), e.crc32, e.name, mark(e))?;
            }
            writeln!(w, "--------          -------  ---                            -------")?;
            let n = selected.len();
            writeln!(w, "{:>8}         {:>8} {:>3}%                            {} file{}", total, ctotal, ratio(total, ctotal), n, if n == 1 { "" } else { "s" })?;
        } else {
            writeln!(w, "  Length      Date    Time    Name")?;
            writeln!(w, "---------  ---------- -----   ----")?;
            for &i in &selected {
                let e = &entries[i];
                total += e.size;
                writeln!(w, "{:>9}  {}   {}{}", e.size, fmt_time(e.mtime), e.name, mark(e))?;
            }
            writeln!(w, "---------                     -------")?;
            let n = selected.len();
            writeln!(w, "{:>9}                     {} file{}", total, n, if n == 1 { "" } else { "s" })?;
        }
        w.flush()
    })();
    match res {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        r => Ok(r?),
    }
}

fn test(g: &Global, a: TestArgs, report: &Reporter, cancel: &AtomicBool) -> Result<()> {
    let input = open_input(&a.archive)?;
    let reader = open_reader(g, &input, a.charset.clone())?;
    let filter = Filter::new(&a.patterns, &a.exclude)?;
    let selected = extract::select(&*reader, &filter);
    extract::test(&*reader, &selected, g.threads(), report, cancel)?;
    report.finish();
    if report.errors() == 0 {
        if !g.quiet {
            println!("No errors detected in compressed data of {}.", a.archive.display());
        }
    } else {
        eprintln!("parch: {} entr{} failed verification in {}", report.errors(), if report.errors() == 1 { "y" } else { "ies" }, a.archive.display());
    }
    Ok(())
}
