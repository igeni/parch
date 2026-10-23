//! Compression method abstraction. Implementations live in `parch-codecs`.

use std::io::{BufRead, Read};

use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodecId {
    Store,
    Deflate,
    Deflate64,
    Bzip2,
    Lzma,
    Xz,
    Zstd,
    Ppmd,
}

impl CodecId {
    pub fn name(self) -> &'static str {
        match self {
            CodecId::Store => "store",
            CodecId::Deflate => "deflate",
            CodecId::Deflate64 => "deflate64",
            CodecId::Bzip2 => "bzip2",
            CodecId::Lzma => "lzma",
            CodecId::Xz => "xz",
            CodecId::Zstd => "zstd",
            CodecId::Ppmd => "ppmd",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    /// The codec can compress.
    pub encode: bool,
    /// One stream can be produced from independently compressed chunks (pigz style).
    pub parallel_chunks: bool,
}

/// Per-thread compressor state; reused across entries.
pub trait Encoder: Send {
    /// Compresses `input` as one complete stream appended to `out`.
    /// Returns `Ok(false)` (leaving `out` unchanged) when the result would not be smaller than `input`.
    fn compress(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<bool>;

    /// Compresses one chunk of a stream split into chunks: `dict` is the data preceding `input`
    /// (used as a preset dictionary), `last` marks the final chunk. Chunk outputs concatenated
    /// in order form one valid stream.
    fn compress_chunk(&mut self, dict: &[u8], input: &[u8], last: bool, out: &mut Vec<u8>) -> Result<()>;
}

/// Codec-specific decoding parameters supplied by the archive format.
#[derive(Clone, Copy, Debug, Default)]
pub struct DecodeParams<'a> {
    pub unpacked_size: u64,
    /// Codec properties (e.g. LZMA props byte + dictionary size, PPMd order/memory/restore).
    pub props: &'a [u8],
}

pub trait Codec: Send + Sync {
    fn id(&self) -> CodecId;
    fn caps(&self) -> Caps;
    /// `None` when the codec is decode-only.
    fn encoder(&self, level: u32) -> Option<Box<dyn Encoder>>;
    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>>;
    /// Optional fast path: decode a complete buffer of known unpacked size.
    fn decode_whole(&self, _input: &[u8], _unpacked_size: usize) -> Option<Result<Vec<u8>>> {
        None
    }
}
