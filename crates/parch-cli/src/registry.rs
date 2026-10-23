//! Registered archive formats. A new format is one more entry in `FORMATS`.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Result};
use parch_core::format::ArchiveFormat;

static FORMATS: &[&dyn ArchiveFormat] = &[&parch_zip::ZIP];

fn by_id(id: &str) -> Result<&'static dyn ArchiveFormat> {
    match FORMATS.iter().find(|f| f.id().eq_ignore_ascii_case(id)) {
        Some(f) => Ok(*f),
        None => bail!("unknown format: {id} (available: {})", FORMATS.iter().map(|f| f.id()).collect::<Vec<_>>().join(", ")),
    }
}

fn by_extension(path: &Path) -> Option<&'static dyn ArchiveFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    FORMATS.iter().find(|f| f.extensions().contains(&ext.as_str())).copied()
}

/// Format for a new archive: `--format`, else by extension, else the first (ZIP).
pub fn for_create(path: &Path, forced: Option<&str>) -> Result<&'static dyn ArchiveFormat> {
    match forced {
        Some(id) => by_id(id),
        None => Ok(by_extension(path).unwrap_or(FORMATS[0])),
    }
}

/// Format of an existing archive: `--format`, else by signature, else by extension, else ZIP
/// (archives with prefixed data are recognized by their trailer).
pub fn for_open(path: &Path, forced: Option<&str>) -> Result<&'static dyn ArchiveFormat> {
    if let Some(id) = forced {
        return by_id(id);
    }
    let mut head = [0u8; 16];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if let Some(f) = FORMATS.iter().find(|f| f.detect(&head[..n])) {
        return Ok(*f);
    }
    Ok(by_extension(path).unwrap_or(FORMATS[0]))
}
