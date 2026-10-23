//! ZIP reader: locates the central directory (ZIP64, prefixed/SFX archives), parses it into
//! entries and decodes entry data with positional reads, so one reader serves many threads.

use std::any::Any;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::ops::Range;
use std::path::Path;

use parch_core::codec::{CodecId, DecodeParams};
use parch_core::entry::{EntryKind, S_IFDIR, S_IFLNK, S_IFMT};
use parch_core::format::{ArchiveReader, EntryInfo, OpenOpts};
use parch_core::fsmeta::{read_at, read_exact_at};
use parch_core::{Error, IoResultExt, Result};

use crate::extra;
use crate::methods;
use crate::names::{self, Charset};
use crate::spec::*;

/// Entries up to this size are decoded from one in-memory buffer.
const WHOLE_LIMIT: u64 = 64 << 20;

/// Central directory fields kept for raw copies.
pub(crate) struct Raw {
    pub method: u16,
    pub flags: u16,
    pub made_by: u16,
    pub needed: u16,
    pub time: u16,
    pub date: u16,
    pub int_attrs: u16,
    pub ext_attrs: u32,
    /// Absolute offset of the local header (prefix data accounted for).
    pub offset: u64,
    pub name: Range<usize>,
    pub extra: Range<usize>,
    pub comment: Range<usize>,
}

pub struct ZipReader {
    file: File,
    len: u64,
    cd: Vec<u8>,
    raw: Vec<Raw>,
    entries: Vec<EntryInfo>,
    comment: Vec<u8>,
}

fn bad(msg: &str) -> Error {
    Error::format(msg)
}

fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < EOCD_LEN {
        return None;
    }
    let sig = EOCD_SIG.to_le_bytes();
    let mut fallback = None;
    for i in (0..=tail.len() - EOCD_LEN).rev() {
        if tail[i..i + 4] == sig {
            let clen = u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize;
            if i + EOCD_LEN + clen == tail.len() {
                return Some(i);
            }
            if fallback.is_none() && i + EOCD_LEN + clen <= tail.len() {
                fallback = Some(i);
            }
        }
    }
    fallback
}

impl ZipReader {
    pub fn open(path: &Path, opts: &OpenOpts) -> Result<Self> {
        let charset = match opts.charset.as_deref() {
            Some(c) => names::parse_charset(c)?,
            None => None,
        };
        let file = File::open(path).at(path)?;
        let len = file.metadata().at(path)?.len();
        Self::parse(file, len, charset.as_ref()).map_err(|e| match e {
            Error::Format(m) => Error::format(format!("{}: {m}", path.display())),
            e => e,
        })
    }

    fn parse(file: File, len: u64, charset: Option<&Charset>) -> Result<Self> {
        if len < EOCD_LEN as u64 {
            return Err(bad("not a zip archive"));
        }
        let tail_len = len.min((EOCD_LEN + 0xFFFF + ZIP64_LOCATOR_LEN) as u64) as usize;
        let tail_start = len - tail_len as u64;
        let mut tail = vec![0u8; tail_len];
        read_exact_at(&file, &mut tail, tail_start)?;
        let at = find_eocd(&tail).ok_or_else(|| bad("not a zip archive (no end of central directory record)"))?;
        let eocd_pos = tail_start + at as u64;
        let mut r = Le::new(&tail[at + 4..]);
        let (disk, cd_disk) = (r.u16().unwrap_or(0), r.u16().unwrap_or(0));
        let _entries_disk = r.u16();
        let mut total = r.u16().unwrap_or(0) as u64;
        let mut cd_size = r.u32().unwrap_or(0) as u64;
        let mut cd_off = r.u32().unwrap_or(0) as u64;
        let clen = r.u16().unwrap_or(0) as usize;
        let comment = tail[at + EOCD_LEN..(at + EOCD_LEN + clen).min(tail.len())].to_vec();

        let mut cd_end = eocd_pos;
        let mut zip64 = false;
        if eocd_pos >= ZIP64_LOCATOR_LEN as u64 {
            let loc_pos = eocd_pos - ZIP64_LOCATOR_LEN as u64;
            let mut loc = [0u8; ZIP64_LOCATOR_LEN];
            read_exact_at(&file, &mut loc, loc_pos)?;
            let mut l = Le::new(&loc);
            if l.u32() == Some(ZIP64_LOCATOR_SIG) {
                l.u32();
                let declared = l.u64().unwrap_or(0);
                let mut found = None;
                for cand in [declared, loc_pos.saturating_sub(ZIP64_EOCD_LEN as u64)] {
                    if cand + ZIP64_EOCD_LEN as u64 > loc_pos {
                        continue;
                    }
                    let mut z = [0u8; ZIP64_EOCD_LEN];
                    read_exact_at(&file, &mut z, cand)?;
                    if z[..4] == ZIP64_EOCD_SIG.to_le_bytes() {
                        found = Some((cand, z));
                        break;
                    }
                }
                let (zpos, z) = found.ok_or_else(|| bad("ZIP64 end of central directory record not found"))?;
                let mut zr = Le::new(&z[4..]);
                zr.u64();
                zr.u16();
                zr.u16();
                let (d, cdd) = (zr.u32().unwrap_or(0), zr.u32().unwrap_or(0));
                zr.u64();
                total = zr.u64().unwrap_or(0);
                cd_size = zr.u64().unwrap_or(0);
                cd_off = zr.u64().unwrap_or(0);
                if d != 0 || cdd != 0 {
                    return Err(Error::Unsupported("multi-volume archives".into()));
                }
                cd_end = zpos;
                zip64 = true;
            }
        }
        if !zip64 && (disk != 0 || cd_disk != 0) {
            return Err(Error::Unsupported("multi-volume archives".into()));
        }
        if cd_size > cd_end {
            return Err(bad("central directory size is out of range"));
        }
        let cd_start = cd_end - cd_size;
        if cd_off > cd_start {
            return Err(bad("central directory offset is out of range"));
        }
        let shift = cd_start - cd_off;
        let mut cd = vec![0u8; cd_size as usize];
        read_exact_at(&file, &mut cd, cd_start)?;

        let cap = total.min(cd_size / CDH_LEN as u64) as usize;
        let mut raw = Vec::with_capacity(cap);
        let mut entries = Vec::with_capacity(cap);
        let mut pos = 0usize;
        while pos + CDH_LEN <= cd.len() {
            let mut h = Le::new(&cd[pos..pos + CDH_LEN]);
            if h.u32() != Some(CDH_SIG) {
                break;
            }
            let v: Vec<u16> = (0..6).map(|_| h.u16().unwrap_or(0)).collect();
            let (made_by, needed, flags, method, time, date) = (v[0], v[1], v[2], v[3], v[4], v[5]);
            let crc = h.u32().unwrap_or(0);
            let csize = h.u32().unwrap_or(0);
            let usize_ = h.u32().unwrap_or(0);
            let nlen = h.u16().unwrap_or(0) as usize;
            let elen = h.u16().unwrap_or(0) as usize;
            let clen = h.u16().unwrap_or(0) as usize;
            h.u16();
            let int_attrs = h.u16().unwrap_or(0);
            let ext_attrs = h.u32().unwrap_or(0);
            let offset = h.u32().unwrap_or(0);
            let name = pos + CDH_LEN..pos + CDH_LEN + nlen;
            let extra_r = name.end..name.end + elen;
            let comment_r = extra_r.end..extra_r.end + clen;
            if comment_r.end > cd.len() {
                return Err(bad("truncated central directory"));
            }
            let (mut size, mut csize64, mut off64) = (usize_ as u64, csize as u64, offset as u64);
            let need = [usize_ == u32::MAX, csize == u32::MAX, offset == u32::MAX];
            let extra_data = &cd[extra_r.clone()];
            if !extra::zip64_values(extra_data, &mut size, &mut csize64, &mut off64, need) {
                return Err(bad("invalid ZIP64 extra field"));
            }
            let offset = off64.checked_add(shift).filter(|o| o + LFH_LEN as u64 <= cd_start).ok_or_else(|| bad("local header offset is out of range"))?;
            if csize64 > len {
                return Err(bad("compressed size is out of range"));
            }
            let p = extra::parse(extra_data);
            let host = (made_by >> 8) as u8;
            let raw_name = &cd[name.clone()];
            let mut display = names::decode(raw_name, flags, p.unicode_path.as_ref(), charset);
            if is_dos_host(host) && display.contains('\\') && !display.contains('/') {
                display = display.replace('\\', "/");
            }
            let mode = (is_unix_host(host) && ext_attrs >> 16 != 0).then_some(ext_attrs >> 16);
            let kind = if display.ends_with('/') || mode.is_some_and(|m| m & S_IFMT == S_IFDIR) || (mode.is_none() && ext_attrs & 0x10 != 0 && size == 0) {
                EntryKind::Dir
            } else if mode.is_some_and(|m| m & S_IFMT == S_IFLNK) {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            let encrypted = flags & FLAG_ENCRYPTED != 0 || method == methods::AES;
            let real_method = if method == methods::AES { p.aes_method.unwrap_or(method) } else { method };
            entries.push(EntryInfo {
                name: display,
                raw_name: raw_name.to_vec(),
                kind,
                codec: methods::codec(real_method),
                size,
                compressed_size: csize64,
                crc32: crc,
                mtime: p.mtime.or_else(|| from_dos(time, date)),
                atime: p.atime,
                btime: p.btime,
                mode,
                uid: p.uid,
                gid: p.gid,
                dos_attrs: is_dos_host(host).then_some(ext_attrs & 0xff),
                encrypted,
            });
            pos = comment_r.end;
            raw.push(Raw { method, flags, made_by, needed, time, date, int_attrs, ext_attrs, offset, name, extra: extra_r, comment: comment_r });
        }
        if (entries.len() as u64) < total && (zip64 || total != U16_MAX) {
            return Err(bad("central directory is truncated"));
        }
        Ok(Self { file, len, cd, raw, entries, comment })
    }

    pub(crate) fn raw(&self, i: usize) -> &Raw {
        &self.raw[i]
    }

    pub(crate) fn cd_bytes(&self, r: &Range<usize>) -> &[u8] {
        &self.cd[r.clone()]
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    fn local_header(&self, i: usize) -> Result<(u64, usize, usize)> {
        let off = self.raw[i].offset;
        let mut h = [0u8; LFH_LEN];
        read_exact_at(&self.file, &mut h, off).map_err(|_| bad("local header is out of range"))?;
        let (nlen, elen) = parse_lfh(&h)?;
        let start = off + (LFH_LEN + nlen + elen) as u64;
        if start + self.entries[i].compressed_size > self.len {
            return Err(bad("entry data is out of range"));
        }
        Ok((start, nlen, elen))
    }

    /// Byte span of an entry's local record (header, data, data descriptor).
    pub(crate) fn span(&self, i: usize) -> Result<Range<u64>> {
        let (start, nlen, elen) = self.local_header(i)?;
        let r = &self.raw[i];
        let e = &self.entries[i];
        let mut end = start + e.compressed_size;
        if r.flags & FLAG_DESCRIPTOR != 0 {
            let mut ex = vec![0u8; elen];
            read_exact_at(&self.file, &mut ex, r.offset + (LFH_LEN + nlen) as u64)?;
            let zip64 = extra::fields(&ex).any(|(id, _)| id == extra::ZIP64) || e.compressed_size >= U32_MAX || e.size >= U32_MAX;
            let mut sig = [0u8; 4];
            let has_sig = read_exact_at(&self.file, &mut sig, end).is_ok() && sig == DD_SIG.to_le_bytes();
            end += if has_sig { 4 } else { 0 } + 4 + if zip64 { 16 } else { 8 };
            if end > self.len {
                return Err(bad("data descriptor is out of range"));
            }
        }
        Ok(r.offset..end)
    }

    fn check(&self, i: usize) -> Result<CodecId> {
        let e = &self.entries[i];
        if e.encrypted {
            return Err(Error::Unsupported("encrypted entry".into()));
        }
        let id = e.codec.ok_or_else(|| Error::Unsupported(format!("compression method {}", self.raw[i].method)))?;
        if parch_codecs::get(id).is_none() {
            return Err(Error::Unsupported(format!("compression method {} is not compiled in", id.name())));
        }
        if id == CodecId::Store && e.size != e.compressed_size {
            return Err(bad("stored entry sizes differ"));
        }
        Ok(id)
    }

    fn decoder<'a>(&self, i: usize, mut input: Box<dyn BufRead + Send + 'a>) -> Result<Box<dyn Read + Send + 'a>> {
        let id = self.check(i)?;
        let codec = parch_codecs::get(id).expect("checked");
        let mut props = Vec::new();
        match self.raw[i].method {
            methods::LZMA => {
                let mut h = [0u8; 4];
                input.read_exact(&mut h)?;
                if u16::from_le_bytes([h[2], h[3]]) != 5 {
                    return Err(bad("unexpected LZMA properties size"));
                }
                props.resize(5, 0);
                input.read_exact(&mut props)?;
            }
            methods::PPMD => {
                props.resize(2, 0);
                input.read_exact(&mut props)?;
            }
            _ => {}
        }
        codec.decoder(input, &DecodeParams { unpacked_size: self.entries[i].size, props: &props })
    }
}

fn parse_lfh(h: &[u8]) -> Result<(usize, usize)> {
    let mut r = Le::new(h);
    if r.u32() != Some(LFH_SIG) {
        return Err(bad("bad local header signature"));
    }
    let mut r = Le::new(&h[26..LFH_LEN]);
    Ok((r.u16().unwrap_or(0) as usize, r.u16().unwrap_or(0) as usize))
}

struct Positional<'a> {
    file: &'a File,
    pos: u64,
    end: u64,
}

impl Read for Positional<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (self.end - self.pos).min(buf.len() as u64) as usize;
        if n == 0 {
            return Ok(0);
        }
        let got = read_at(self.file, &mut buf[..n], self.pos)?;
        self.pos += got as u64;
        Ok(got)
    }
}

/// Limits output to the declared size and verifies the CRC-32 when it is reached.
struct Verify<R> {
    inner: R,
    remaining: u64,
    crc: crc32fast::Hasher,
    expected: u32,
    checked: bool,
}

impl<R: Read> Verify<R> {
    fn finish(&mut self) -> io::Result<()> {
        self.checked = true;
        let crc = std::mem::take(&mut self.crc).finalize();
        if crc != self.expected {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("CRC mismatch (expected {:08x}, got {crc:08x})", self.expected)));
        }
        Ok(())
    }
}

impl<R: Read> Read for Verify<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            if !self.checked {
                self.finish()?;
            }
            return Ok(0);
        }
        let want = buf.len().min(self.remaining.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "entry data ended early"));
        }
        self.crc.update(&buf[..n]);
        self.remaining -= n as u64;
        if self.remaining == 0 {
            self.finish()?;
        }
        Ok(n)
    }
}

impl ArchiveReader for ZipReader {
    fn entries(&self) -> &[EntryInfo] {
        &self.entries
    }

    fn open_entry(&self, index: usize) -> Result<Box<dyn Read + Send + '_>> {
        self.check(index)?;
        let e = &self.entries[index];
        let (start, _, _) = self.local_header(index)?;
        let raw = Positional { file: &self.file, pos: start, end: start + e.compressed_size };
        let dec = self.decoder(index, Box::new(BufReader::with_capacity(256 << 10, raw)))?;
        Ok(Box::new(Verify { inner: dec, remaining: e.size, crc: crc32fast::Hasher::new(), expected: e.crc32, checked: false }))
    }

    fn read_entry(&self, index: usize) -> Result<Vec<u8>> {
        let id = self.check(index)?;
        let e = &self.entries[index];
        if e.size > WHOLE_LIMIT || e.compressed_size > WHOLE_LIMIT {
            let mut v = Vec::new();
            self.open_entry(index)?.read_to_end(&mut v)?;
            return Ok(v);
        }
        let r = &self.raw[index];
        let csize = e.compressed_size as usize;
        let guess = (LFH_LEN + r.name.len() + r.extra.len() + csize + 64) as u64;
        let avail = (self.len - r.offset).min(guess) as usize;
        let mut buf = vec![0u8; avail];
        read_exact_at(&self.file, &mut buf, r.offset)?;
        if buf.len() < LFH_LEN {
            return Err(bad("local header is out of range"));
        }
        let (nlen, elen) = parse_lfh(&buf[..LFH_LEN])?;
        let hdr = LFH_LEN + nlen + elen;
        let owned;
        let data: &[u8] = if hdr + csize <= buf.len() {
            &buf[hdr..hdr + csize]
        } else {
            let start = r.offset + hdr as u64;
            if start + e.compressed_size > self.len {
                return Err(bad("entry data is out of range"));
            }
            let mut d = vec![0u8; csize];
            read_exact_at(&self.file, &mut d, start)?;
            owned = d;
            &owned
        };
        let out = match id {
            CodecId::Store | CodecId::Deflate => parch_codecs::get(id).expect("codec").decode_whole(data, e.size as usize).expect("whole decode")?,
            _ => {
                let mut dec = self.decoder(index, Box::new(io::Cursor::new(data)))?;
                let mut out = Vec::with_capacity(e.size as usize);
                (&mut dec).take(e.size).read_to_end(&mut out)?;
                if out.len() as u64 != e.size {
                    return Err(bad("entry data ended early"));
                }
                out
            }
        };
        let crc = crc32fast::hash(&out);
        if crc != e.crc32 {
            return Err(Error::Io(io::Error::new(io::ErrorKind::InvalidData, format!("CRC mismatch (expected {:08x}, got {crc:08x})", e.crc32))));
        }
        Ok(out)
    }

    fn method_label(&self, index: usize) -> String {
        let r = &self.raw[index];
        methods::label(r.method, r.flags)
    }

    fn comment(&self) -> &[u8] {
        &self.comment
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
