//! ZIP writer. Entries arrive compressed, so local headers carry final CRC and sizes (no data
//! descriptors) except for chunked entries on non-seekable outputs. Chunked entries on files get
//! their local header patched once the last chunk is written. ZIP64 structures are emitted only
//! when needed (or always with `force_zip64`).

use std::io::Write;
use std::ops::Range;

use parch_core::codec::CodecId;
use parch_core::entry::{EntryKind, EntryMeta, S_IFMT};
use parch_core::format::{ArchiveWriter, CreateOpts, Summary};
use parch_core::fsmeta::read_exact_at;
use parch_core::sink::Sink;
use parch_core::{Error, Result};

use crate::extra;
use crate::methods;
use crate::names::{self, Charset};
use crate::reader::ZipReader;
use crate::spec::*;

/// Streamed entries announced with at least this size get ZIP64 local headers up front
/// (deflate output may exceed the input slightly).
const ZIP64_STREAM_THRESHOLD: u64 = 0xF000_0000;
const DOS_DIR: u32 = 0x10;
const DOS_READONLY: u32 = 0x01;

struct Record {
    made_by: u16,
    needed: u16,
    flags: u16,
    method: u16,
    time: u16,
    date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
    int_attrs: u16,
    ext_attrs: u32,
    name: Range<usize>,
    extra: Range<usize>,
    comment: Range<usize>,
}

struct Open {
    rec: Record,
    lfh_offset: u64,
    name_len: usize,
    zip64: bool,
    descriptor: bool,
    csize: u64,
}

pub struct ZipWriter {
    sink: Box<dyn Sink>,
    pos: u64,
    opts: CreateOpts,
    charset: Option<Charset>,
    records: Vec<Record>,
    /// Records raw-copied from a base archive; they keep their order.
    copied: usize,
    arena: Vec<u8>,
    cur: Option<Open>,
    summary: Summary,
}

impl ZipWriter {
    pub fn new(sink: Box<dyn Sink>, opts: &CreateOpts) -> Result<Self> {
        if opts.comment.len() > u16::MAX as usize {
            return Err(Error::msg("archive comment is longer than 65535 bytes"));
        }
        let charset = match opts.charset.as_deref() {
            Some(c) => names::parse_charset(c)?,
            None => None,
        };
        Ok(Self { sink, pos: 0, opts: opts.clone(), charset, records: Vec::new(), copied: 0, arena: Vec::new(), cur: None, summary: Summary::default() })
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.sink.write_all(data)?;
        self.pos += data.len() as u64;
        Ok(())
    }

    fn push_arena(&mut self, data: &[u8]) -> Range<usize> {
        let start = self.arena.len();
        self.arena.extend_from_slice(data);
        start..self.arena.len()
    }

    /// Builds the local header and the central record for an entry.
    fn prepare(&mut self, meta: &EntryMeta, codec: CodecId, crc: u32, csize: u64, usize: u64, zip64: bool, descriptor: bool) -> (Vec<u8>, Record, usize) {
        let mut name = meta.name.clone();
        if meta.kind == EntryKind::Dir {
            name.push(b'/');
        }
        let enc = names::encode(name, self.charset.as_ref());
        let method = methods::method(codec);
        let mut flags = 0u16;
        if enc.utf8_flag {
            flags |= FLAG_UTF8;
        }
        if descriptor {
            flags |= FLAG_DESCRIPTOR;
        }
        let unix = meta.mode.is_some();
        let made_by = (u16::from(if unix { HOST_UNIX } else { HOST_DOS }) << 8) | u16::from(SPEC_VERSION);
        let ext_attrs = match meta.mode {
            Some(mode) => {
                let mut a = mode << 16;
                if meta.kind == EntryKind::Dir || mode & S_IFMT == parch_core::entry::S_IFDIR {
                    a |= DOS_DIR;
                }
                if mode & 0o200 == 0 {
                    a |= DOS_READONLY;
                }
                a
            }
            None => {
                let mut a = meta.dos_attrs.unwrap_or(0) & 0x37;
                if meta.kind == EntryKind::Dir {
                    a |= DOS_DIR;
                }
                a
            }
        };
        let mut needed = methods::version_needed(method);
        if meta.kind == EntryKind::Dir {
            needed = needed.max(20);
        }
        if zip64 {
            needed = needed.max(VERSION_ZIP64);
        }
        let (time, date) = to_dos(meta.mtime, self.opts.utc_times);

        let mut local_extra = Vec::new();
        let mut central_extra = Vec::new();
        if zip64 {
            extra::write_zip64(&mut local_extra, &[usize, csize]);
        }
        // Access times are not stored: reading the inputs changes them, which would make
        // otherwise identical archives differ.
        if self.opts.extra_fields {
            if unix {
                extra::write_ext_time(&mut local_extra, meta.mtime, None, true);
                extra::write_ext_time(&mut central_extra, meta.mtime, None, false);
                if let (Some(uid), Some(gid)) = (meta.uid, meta.gid) {
                    extra::write_unix(&mut local_extra, uid, gid);
                    extra::write_unix(&mut central_extra, uid, gid);
                }
            } else if let Some(m) = meta.mtime {
                extra::write_ntfs(&mut local_extra, m, None, meta.btime);
                extra::write_ntfs(&mut central_extra, m, None, meta.btime);
            }
        }
        if let Some(u) = &enc.unicode_path {
            extra::write_unicode_path(&mut local_extra, &enc.raw, u);
            extra::write_unicode_path(&mut central_extra, &enc.raw, u);
        }

        let mut lfh = Vec::with_capacity(LFH_LEN + enc.raw.len() + local_extra.len());
        put32(&mut lfh, LFH_SIG);
        put16(&mut lfh, needed);
        put16(&mut lfh, flags);
        put16(&mut lfh, method);
        put16(&mut lfh, time);
        put16(&mut lfh, date);
        put32(&mut lfh, if descriptor { 0 } else { crc });
        if zip64 {
            put32(&mut lfh, u32::MAX);
            put32(&mut lfh, u32::MAX);
        } else {
            put32(&mut lfh, csize as u32);
            put32(&mut lfh, usize as u32);
        }
        put16(&mut lfh, enc.raw.len() as u16);
        put16(&mut lfh, local_extra.len() as u16);
        lfh.extend_from_slice(&enc.raw);
        lfh.extend_from_slice(&local_extra);

        let name = self.push_arena(&enc.raw);
        let extra = self.push_arena(&central_extra);
        let rec = Record {
            made_by,
            needed,
            flags,
            method,
            time,
            date,
            crc,
            csize,
            usize,
            offset: self.pos,
            int_attrs: 0,
            ext_attrs,
            name,
            extra,
            comment: 0..0,
        };
        (lfh, rec, enc.raw.len())
    }

    /// Copies the selected entries of `base` verbatim (local records and central fields).
    pub fn copy_from(&mut self, base: &ZipReader, keep: &dyn Fn(usize) -> bool) -> Result<()> {
        use parch_core::format::ArchiveReader;
        let n = base.entries().len();
        let mut spans = Vec::new();
        for i in (0..n).filter(|&i| keep(i)) {
            spans.push((i, base.span(i)?));
        }
        let mut buf = vec![0u8; 1 << 20];
        let mut k = 0;
        while k < spans.len() {
            // Merge entries that are adjacent in the source.
            let start = spans[k].1.start;
            let mut end = spans[k].1.end;
            let mut j = k + 1;
            while j < spans.len() && spans[j].1.start == end {
                end = spans[j].1.end;
                j += 1;
            }
            let dest = self.pos;
            for (i, span) in &spans[k..j] {
                let r = base.raw(*i);
                let e = &base.entries()[*i];
                let mut central_extra = Vec::new();
                extra::without(base.cd_bytes(&r.extra), extra::ZIP64, &mut central_extra);
                let name = self.push_arena(base.cd_bytes(&r.name));
                let extra_r = self.push_arena(&central_extra);
                let comment = self.push_arena(base.cd_bytes(&r.comment));
                self.records.push(Record {
                    made_by: r.made_by,
                    needed: r.needed,
                    flags: r.flags,
                    method: r.method,
                    time: r.time,
                    date: r.date,
                    crc: e.crc32,
                    csize: e.compressed_size,
                    usize: e.size,
                    offset: dest + (span.start - start),
                    int_attrs: r.int_attrs,
                    ext_attrs: r.ext_attrs,
                    name,
                    extra: extra_r,
                    comment,
                });
                self.summary.entries += 1;
                self.summary.bytes_in += e.size;
                self.summary.bytes_out += e.compressed_size;
            }
            let mut off = start;
            while off < end {
                let n = ((end - off) as usize).min(buf.len());
                read_exact_at(base.file(), &mut buf[..n], off)?;
                self.write(&buf[..n])?;
                off += n as u64;
            }
            k = j;
        }
        self.copied = self.records.len();
        Ok(())
    }

    fn write_central(&mut self) -> Result<(u64, u64)> {
        let force = self.opts.force_zip64;
        let cd_start = self.pos;
        let mut out = Vec::with_capacity(1 << 20);
        let records = std::mem::take(&mut self.records);
        for r in &records {
            let mut z = Vec::new();
            let big_u = force || r.usize >= U32_MAX;
            let big_c = force || r.csize >= U32_MAX;
            let big_o = force || r.offset >= U32_MAX;
            if big_u {
                z.push(r.usize);
            }
            if big_c {
                z.push(r.csize);
            }
            if big_o {
                z.push(r.offset);
            }
            let mut ex = Vec::new();
            extra::write_zip64(&mut ex, &z);
            ex.extend_from_slice(&self.arena[r.extra.clone()]);
            let ex_len = ex.len().min(u16::MAX as usize);
            let needed = if z.is_empty() { r.needed } else { r.needed.max(VERSION_ZIP64) };
            put32(&mut out, CDH_SIG);
            put16(&mut out, r.made_by);
            put16(&mut out, needed);
            put16(&mut out, r.flags);
            put16(&mut out, r.method);
            put16(&mut out, r.time);
            put16(&mut out, r.date);
            put32(&mut out, r.crc);
            put32(&mut out, if big_c { u32::MAX } else { r.csize as u32 });
            put32(&mut out, if big_u { u32::MAX } else { r.usize as u32 });
            put16(&mut out, r.name.len() as u16);
            put16(&mut out, ex_len as u16);
            put16(&mut out, r.comment.len() as u16);
            put16(&mut out, 0);
            put16(&mut out, r.int_attrs);
            put32(&mut out, r.ext_attrs);
            put32(&mut out, if big_o { u32::MAX } else { r.offset as u32 });
            out.extend_from_slice(&self.arena[r.name.clone()]);
            out.extend_from_slice(&ex[..ex_len]);
            out.extend_from_slice(&self.arena[r.comment.clone()]);
            if out.len() >= 1 << 20 {
                self.write(&out)?;
                out.clear();
            }
        }
        self.write(&out)?;
        self.records = records;
        Ok((cd_start, self.pos - cd_start))
    }
}

impl ArchiveWriter for ZipWriter {
    fn add(&mut self, meta: &EntryMeta, codec: CodecId, crc32: u32, data: &[u8]) -> Result<()> {
        if self.cur.is_some() {
            return Err(Error::msg("internal error: add while a chunked entry is open"));
        }
        let (csize, usize) = (data.len() as u64, meta.size);
        let zip64 = self.opts.force_zip64 || csize >= U32_MAX || usize >= U32_MAX;
        let (lfh, rec, _) = self.prepare(meta, codec, crc32, csize, usize, zip64, false);
        self.write(&lfh)?;
        self.write(data)?;
        self.records.push(rec);
        self.summary.entries += 1;
        self.summary.bytes_in += usize;
        self.summary.bytes_out += csize;
        Ok(())
    }

    fn begin(&mut self, meta: &EntryMeta, codec: CodecId) -> Result<()> {
        if self.cur.is_some() {
            return Err(Error::msg("internal error: nested chunked entries"));
        }
        let descriptor = !self.sink.seekable();
        let zip64 = self.opts.force_zip64 || meta.size >= ZIP64_STREAM_THRESHOLD;
        let lfh_offset = self.pos;
        let (lfh, rec, name_len) = self.prepare(meta, codec, 0, 0, 0, zip64, descriptor);
        self.write(&lfh)?;
        self.cur = Some(Open { rec, lfh_offset, name_len, zip64, descriptor, csize: 0 });
        Ok(())
    }

    fn chunk(&mut self, data: &[u8]) -> Result<()> {
        self.write(data)?;
        let cur = self.cur.as_mut().ok_or_else(|| Error::msg("internal error: chunk without entry"))?;
        cur.csize += data.len() as u64;
        Ok(())
    }

    fn end(&mut self, crc32: u32, size: u64) -> Result<()> {
        let mut cur = self.cur.take().ok_or_else(|| Error::msg("internal error: end without entry"))?;
        let csize = cur.csize;
        if !cur.zip64 && (csize >= U32_MAX || size >= U32_MAX) {
            return Err(Error::msg(format!(
                "{}: entry grew beyond 4 GiB while archiving; retry with --zip64 always",
                String::from_utf8_lossy(&self.arena[cur.rec.name.clone()])
            )));
        }
        if cur.descriptor {
            let mut dd = Vec::with_capacity(24);
            put32(&mut dd, DD_SIG);
            put32(&mut dd, crc32);
            if cur.zip64 {
                put64(&mut dd, csize);
                put64(&mut dd, size);
            } else {
                put32(&mut dd, csize as u32);
                put32(&mut dd, size as u32);
            }
            self.write(&dd)?;
        } else {
            let off = cur.lfh_offset;
            self.sink.patch(off + 14, &crc32.to_le_bytes())?;
            if cur.zip64 {
                let mut sizes = Vec::with_capacity(16);
                put64(&mut sizes, size);
                put64(&mut sizes, csize);
                self.sink.patch(off + (LFH_LEN + cur.name_len + 4) as u64, &sizes)?;
            } else {
                let mut sizes = Vec::with_capacity(8);
                put32(&mut sizes, csize as u32);
                put32(&mut sizes, size as u32);
                self.sink.patch(off + 18, &sizes)?;
            }
        }
        cur.rec.crc = crc32;
        cur.rec.csize = csize;
        cur.rec.usize = size;
        self.records.push(cur.rec);
        self.summary.entries += 1;
        self.summary.bytes_in += size;
        self.summary.bytes_out += csize;
        Ok(())
    }

    fn abort_entry(&mut self) -> Result<()> {
        let cur = self.cur.take().ok_or_else(|| Error::msg("internal error: abort without entry"))?;
        if !self.sink.seekable() {
            return Err(Error::msg("cannot drop a partially written entry from a stream"));
        }
        self.sink.truncate(cur.lfh_offset)?;
        self.pos = cur.lfh_offset;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<Summary> {
        if self.cur.is_some() {
            return Err(Error::msg("internal error: unfinished entry"));
        }
        if self.opts.sort_index {
            let arena = &self.arena;
            self.records[self.copied..].sort_by(|a, b| arena[a.name.clone()].cmp(&arena[b.name.clone()]));
        }
        let (cd_start, cd_size) = self.write_central()?;
        let n = self.records.len() as u64;
        let zip64 = self.opts.force_zip64 || n >= U16_MAX || cd_start >= U32_MAX || cd_size >= U32_MAX;
        let mut tail = Vec::with_capacity(EOCD_LEN + ZIP64_EOCD_LEN + ZIP64_LOCATOR_LEN + self.opts.comment.len());
        if zip64 {
            let z_off = self.pos;
            put32(&mut tail, ZIP64_EOCD_SIG);
            put64(&mut tail, (ZIP64_EOCD_LEN - 12) as u64);
            put16(&mut tail, (u16::from(HOST_UNIX) << 8) | u16::from(SPEC_VERSION));
            put16(&mut tail, VERSION_ZIP64);
            put32(&mut tail, 0);
            put32(&mut tail, 0);
            put64(&mut tail, n);
            put64(&mut tail, n);
            put64(&mut tail, cd_size);
            put64(&mut tail, cd_start);
            put32(&mut tail, ZIP64_LOCATOR_SIG);
            put32(&mut tail, 0);
            put64(&mut tail, z_off);
            put32(&mut tail, 1);
        }
        let n16 = n.min(U16_MAX) as u16;
        put32(&mut tail, EOCD_SIG);
        put16(&mut tail, 0);
        put16(&mut tail, 0);
        put16(&mut tail, n16);
        put16(&mut tail, n16);
        put32(&mut tail, clamp32(cd_size));
        put32(&mut tail, clamp32(cd_start));
        put16(&mut tail, self.opts.comment.len() as u16);
        tail.extend_from_slice(&self.opts.comment);
        self.write(&tail)?;
        self.sink.finish()?;
        self.summary.bytes_out = self.pos;
        Ok(self.summary)
    }
}
