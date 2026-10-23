//! Output targets for archive writers.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};

const BUF_SIZE: usize = 4 << 20;

pub trait Sink: Write + Send {
    /// Whether already written bytes can be patched (regular file) or not (pipe).
    fn seekable(&self) -> bool;
    /// Overwrites bytes at `offset`; the write position stays at the end.
    fn patch(&mut self, offset: u64, data: &[u8]) -> io::Result<()>;
    /// Drops everything after `len`; subsequent writes continue at `len`.
    fn truncate(&mut self, len: u64) -> io::Result<()>;
    /// Flushes buffers and makes data durable where applicable.
    fn finish(&mut self) -> io::Result<()>;
}

pub struct FileSink {
    out: BufWriter<File>,
    pos: u64,
    sync: bool,
}

impl FileSink {
    pub fn new(file: File, sync: bool) -> Self {
        Self { out: BufWriter::with_capacity(BUF_SIZE, file), pos: 0, sync }
    }
}

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.out.write_all(buf)?;
        self.pos += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Sink for FileSink {
    fn seekable(&self) -> bool {
        true
    }

    fn patch(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.out.flush()?;
        let f = self.out.get_mut();
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(data)?;
        f.seek(SeekFrom::Start(self.pos))?;
        Ok(())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.out.flush()?;
        let f = self.out.get_mut();
        f.set_len(len)?;
        f.seek(SeekFrom::Start(len))?;
        self.pos = len;
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        self.out.flush()?;
        if self.sync {
            self.out.get_ref().sync_all()?;
        }
        Ok(())
    }
}

/// Non-seekable sink (stdout or a pipe).
pub struct StreamSink<W: Write + Send> {
    out: BufWriter<W>,
}

impl<W: Write + Send> StreamSink<W> {
    pub fn new(w: W) -> Self {
        Self { out: BufWriter::with_capacity(BUF_SIZE, w) }
    }
}

impl<W: Write + Send> Write for StreamSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.out.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl<W: Write + Send> Sink for StreamSink<W> {
    fn seekable(&self) -> bool {
        false
    }

    fn patch(&mut self, _offset: u64, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "cannot seek in a stream"))
    }

    fn truncate(&mut self, _len: u64) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "cannot truncate a stream"))
    }

    fn finish(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
