//! ZIP format for `parch`: compatible with PKWARE APPNOTE 6.3 readers and writers
//! (Info-ZIP, 7-Zip, libarchive, Python `zipfile`, Windows and macOS built-in tools).

use std::path::Path;

use parch_core::codec::CodecId;
use parch_core::format::{ArchiveFormat, ArchiveReader, ArchiveWriter, CreateOpts, Layout, OpenOpts};
use parch_core::sink::Sink;
use parch_core::{Error, Result};

pub mod extra;
pub mod methods;
pub mod names;
pub mod reader;
pub mod spec;
pub mod writer;

pub use reader::ZipReader;
pub use writer::ZipWriter;

pub struct ZipFormat;

pub static ZIP: ZipFormat = ZipFormat;

impl ArchiveFormat for ZipFormat {
    fn id(&self) -> &'static str {
        "zip"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["zip", "jar", "war", "ear", "apk", "aar", "xpi", "epub", "cbz", "whl", "nupkg", "ipa"]
    }

    fn detect(&self, head: &[u8]) -> bool {
        head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08")
    }

    fn layout(&self) -> Layout {
        Layout::PerEntry
    }

    fn can_write(&self, codec: CodecId) -> bool {
        matches!(codec, CodecId::Store | CodecId::Deflate)
    }

    fn create(&self, sink: Box<dyn Sink>, opts: &CreateOpts) -> Result<Box<dyn ArchiveWriter>> {
        Ok(Box::new(ZipWriter::new(sink, opts)?))
    }

    fn create_from(
        &self,
        base: &dyn ArchiveReader,
        keep: &dyn Fn(usize) -> bool,
        sink: Box<dyn Sink>,
        opts: &CreateOpts,
    ) -> Result<Box<dyn ArchiveWriter>> {
        let base = base.as_any().downcast_ref::<ZipReader>().ok_or_else(|| Error::msg("base archive is not a zip archive"))?;
        let mut w = ZipWriter::new(sink, opts)?;
        w.copy_from(base, keep)?;
        Ok(Box::new(w))
    }

    fn open(&self, path: &Path, opts: &OpenOpts) -> Result<Box<dyn ArchiveReader>> {
        Ok(Box::new(ZipReader::open(path, opts)?))
    }
}
