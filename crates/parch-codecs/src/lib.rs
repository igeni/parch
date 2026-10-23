//! Compression codecs used by `parch` formats.
//!
//! Store and Deflate can encode; the other codecs are decode-only and each sits behind a cargo
//! feature. A new method is one more `Codec` implementation plus an entry in [`get`].

use std::io::{BufRead, Read};

use parch_core::codec::{Caps, Codec, CodecId, DecodeParams, Encoder};
use parch_core::{Error, Result};

mod deflate;

pub use deflate::Deflate;

/// Returns the implementation of `id`, if compiled in.
pub fn get(id: CodecId) -> Option<&'static dyn Codec> {
    match id {
        CodecId::Store => Some(&Store),
        CodecId::Deflate => Some(&Deflate),
        #[cfg(feature = "deflate64")]
        CodecId::Deflate64 => Some(&Deflate64),
        #[cfg(feature = "bzip2")]
        CodecId::Bzip2 => Some(&Bzip2),
        #[cfg(feature = "lzma")]
        CodecId::Lzma => Some(&Lzma),
        #[cfg(feature = "lzma")]
        CodecId::Xz => Some(&Xz),
        #[cfg(feature = "zstd")]
        CodecId::Zstd => Some(&Zstd),
        #[cfg(feature = "ppmd")]
        CodecId::Ppmd => Some(&Ppmd),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

fn decode_err(codec: &str, e: impl std::fmt::Display) -> Error {
    Error::format(format!("{codec}: {e}"))
}

pub struct Store;

impl Codec for Store {
    fn id(&self) -> CodecId {
        CodecId::Store
    }

    fn caps(&self) -> Caps {
        Caps { encode: true, parallel_chunks: true }
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        Ok(input)
    }

    fn decode_whole(&self, input: &[u8], unpacked_size: usize) -> Option<Result<Vec<u8>>> {
        if input.len() != unpacked_size {
            return Some(Err(Error::format("stored entry size mismatch")));
        }
        Some(Ok(input.to_vec()))
    }
}

#[cfg(feature = "deflate64")]
pub struct Deflate64;

#[cfg(feature = "deflate64")]
impl Codec for Deflate64 {
    fn id(&self) -> CodecId {
        CodecId::Deflate64
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        Ok(Box::new(deflate64::Deflate64Decoder::with_buffer(input)))
    }
}

#[cfg(feature = "bzip2")]
pub struct Bzip2;

#[cfg(feature = "bzip2")]
impl Codec for Bzip2 {
    fn id(&self) -> CodecId {
        CodecId::Bzip2
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        Ok(Box::new(bzip2::bufread::BzDecoder::new(input)))
    }
}

/// Raw LZMA; `props` = properties byte followed by the little-endian dictionary size.
#[cfg(feature = "lzma")]
pub struct Lzma;

#[cfg(feature = "lzma")]
impl Codec for Lzma {
    fn id(&self) -> CodecId {
        CodecId::Lzma
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        let p = params.props;
        if p.len() != 5 {
            return Err(Error::format("lzma: bad properties"));
        }
        let dict = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
        let r = lzma_rust2::LzmaReader::new_with_props(input, params.unpacked_size, p[0], dict, None).map_err(|e| decode_err("lzma", e))?;
        Ok(Box::new(r))
    }
}

#[cfg(feature = "lzma")]
pub struct Xz;

#[cfg(feature = "lzma")]
impl Codec for Xz {
    fn id(&self) -> CodecId {
        CodecId::Xz
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        Ok(Box::new(lzma_rust2::XzReader::new(input, true)))
    }
}

#[cfg(feature = "zstd")]
pub struct Zstd;

#[cfg(feature = "zstd")]
impl Codec for Zstd {
    fn id(&self) -> CodecId {
        CodecId::Zstd
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        let d = zstd::stream::read::Decoder::with_buffer(input).map_err(|e| decode_err("zstd", e))?;
        Ok(Box::new(d))
    }
}

/// PPMd variant I rev. 1; `props` = the 16-bit little-endian ZIP parameter word.
#[cfg(feature = "ppmd")]
pub struct Ppmd;

#[cfg(feature = "ppmd")]
impl Codec for Ppmd {
    fn id(&self) -> CodecId {
        CodecId::Ppmd
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn encoder(&self, _level: u32) -> Option<Box<dyn Encoder>> {
        None
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        let p = params.props;
        if p.len() != 2 {
            return Err(Error::format("ppmd: bad parameters"));
        }
        let word = u16::from_le_bytes([p[0], p[1]]);
        let order = u32::from(word & 0x0f) + 1;
        let mem = (u32::from((word >> 4) & 0xff) + 1) << 20;
        let restore = (word >> 12) & 0x0f;
        let d = ppmd_rust::Ppmd8Decoder::new(input, order, mem, restore.into()).map_err(|e| decode_err("ppmd", e))?;
        Ok(Box::new(d))
    }
}
