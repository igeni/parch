//! Archive format abstraction. A format serializes entries; the engine does the rest.

use std::any::Any;
use std::io::Read;
use std::path::Path;

use crate::codec::CodecId;
use crate::entry::{EntryKind, EntryMeta, Time};
use crate::sink::Sink;
use crate::Result;

/// How a format compresses data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Every entry is compressed independently (ZIP); the engine compresses entries in parallel.
    PerEntry,
    /// The whole archive is one compressed stream (tar.*); reserved for future formats.
    Solid,
}

#[derive(Clone, Debug, Default)]
pub struct CreateOpts {
    pub comment: Vec<u8>,
    /// Always emit 64-bit structures.
    pub force_zip64: bool,
    /// Write extended metadata (precise times, uid/gid).
    pub extra_fields: bool,
    /// Compute legacy local-time timestamps in UTC (reproducible output).
    pub utc_times: bool,
    /// Legacy charset for entry names (e.g. `cp866`); `None` means UTF-8.
    pub charset: Option<String>,
    /// Sort the archive index by name when finishing.
    pub sort_index: bool,
}

#[derive(Clone, Debug, Default)]
pub struct OpenOpts {
    /// Charset for names not marked as UTF-8.
    pub charset: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    pub entries: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// An entry of an existing archive.
#[derive(Clone, Debug)]
pub struct EntryInfo {
    /// Decoded name (`/`-separated), used for display, matching and extraction.
    pub name: String,
    pub raw_name: Vec<u8>,
    pub kind: EntryKind,
    /// `None` when the compression method is unknown or unsupported.
    pub codec: Option<CodecId>,
    pub size: u64,
    pub compressed_size: u64,
    pub crc32: u32,
    pub mtime: Option<Time>,
    pub atime: Option<Time>,
    pub btime: Option<Time>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub dos_attrs: Option<u32>,
    pub encrypted: bool,
}

pub trait ArchiveReader: Send + Sync {
    fn entries(&self) -> &[EntryInfo];
    /// Streams the decoded contents; size and checksum are verified at the end.
    fn open_entry(&self, index: usize) -> Result<Box<dyn Read + Send + '_>>;
    /// Decodes the whole entry into memory (fast path for small entries); verified.
    fn read_entry(&self, index: usize) -> Result<Vec<u8>>;
    /// Short human-readable method label for listings.
    fn method_label(&self, index: usize) -> String;
    fn comment(&self) -> &[u8];
    fn as_any(&self) -> &dyn Any;
}

/// Serializes entries into an archive; lives on the writer thread.
pub trait ArchiveWriter: Send {
    /// Adds an entry whose data is already compressed with `codec`.
    fn add(&mut self, meta: &EntryMeta, codec: CodecId, crc32: u32, data: &[u8]) -> Result<()>;
    /// Starts an entry whose compressed data arrives in chunks.
    fn begin(&mut self, meta: &EntryMeta, codec: CodecId) -> Result<()>;
    fn chunk(&mut self, data: &[u8]) -> Result<()>;
    fn end(&mut self, crc32: u32, size: u64) -> Result<()>;
    /// Discards the entry started with `begin` (fails on non-seekable outputs).
    fn abort_entry(&mut self) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<Summary>;
}

pub trait ArchiveFormat: Send + Sync {
    fn id(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    /// Recognizes the format by its first bytes.
    fn detect(&self, head: &[u8]) -> bool;
    fn layout(&self) -> Layout;
    /// Whether entries can be written with `codec`.
    fn can_write(&self, codec: CodecId) -> bool;
    fn create(&self, sink: Box<dyn Sink>, opts: &CreateOpts) -> Result<Box<dyn ArchiveWriter>>;
    /// Starts a new archive that first receives raw copies of the `keep`-selected entries of `base`
    /// (which must have been opened by this format).
    fn create_from(
        &self,
        base: &dyn ArchiveReader,
        keep: &dyn Fn(usize) -> bool,
        sink: Box<dyn Sink>,
        opts: &CreateOpts,
    ) -> Result<Box<dyn ArchiveWriter>>;
    fn open(&self, path: &Path, opts: &OpenOpts) -> Result<Box<dyn ArchiveReader>>;
}
