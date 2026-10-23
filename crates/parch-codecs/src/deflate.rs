//! Deflate: libdeflate for whole buffers, zlib-rs for chunked streams.
//!
//! A big file is split into chunks compressed independently: each chunk uses the 32 KiB that
//! precede it as a preset dictionary and ends with a sync flush (byte-aligned, not final), the
//! last chunk finishes the stream. Concatenated in order, the chunks form one valid raw deflate
//! stream, as in pigz.

use std::cell::RefCell;
use std::io::{BufRead, Read};

use libdeflater::{CompressionLvl, Compressor, DecompressionError, Decompressor};
use libz_rs_sys as z;
use parch_core::codec::{Caps, Codec, CodecId, DecodeParams, Encoder};
use parch_core::{Error, Result};

pub struct Deflate;

impl Codec for Deflate {
    fn id(&self) -> CodecId {
        CodecId::Deflate
    }

    fn caps(&self) -> Caps {
        Caps { encode: true, parallel_chunks: true }
    }

    fn encoder(&self, level: u32) -> Option<Box<dyn Encoder>> {
        Some(Box::new(DeflateEncoder { level: level.clamp(1, 12), whole: None, chunked: None }))
    }

    fn decoder<'a>(&self, input: Box<dyn BufRead + Send + 'a>, _params: &DecodeParams<'_>) -> Result<Box<dyn Read + Send + 'a>> {
        Ok(Box::new(flate2::bufread::DeflateDecoder::new(input)))
    }

    fn decode_whole(&self, input: &[u8], unpacked_size: usize) -> Option<Result<Vec<u8>>> {
        Some(inflate_whole(input, unpacked_size))
    }
}

thread_local! {
    static INFLATER: RefCell<Option<Decompressor>> = const { RefCell::new(None) };
}

fn inflate_whole(input: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; size];
    INFLATER.with(|cell| {
        let mut cell = cell.borrow_mut();
        let d = cell.get_or_insert_with(Decompressor::new);
        match d.deflate_decompress(input, &mut out) {
            Ok(n) if n == size => Ok(out),
            Ok(n) => Err(Error::format(format!("deflate data is shorter than declared ({n} of {size} bytes)"))),
            Err(DecompressionError::InsufficientSpace) => Err(Error::format("deflate data is longer than declared")),
            Err(DecompressionError::BadData) => Err(Error::format("corrupt deflate data")),
        }
    })
}

pub struct DeflateEncoder {
    level: u32,
    whole: Option<Compressor>,
    chunked: Option<ZStream>,
}

impl Encoder for DeflateEncoder {
    fn compress(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<bool> {
        if input.len() < 2 {
            return Ok(false);
        }
        let level = self.level;
        let c = self.whole.get_or_insert_with(|| Compressor::new(CompressionLvl::new(level as i32).expect("level 1..=12")));
        let start = out.len();
        out.resize(start + input.len() - 1, 0);
        match c.deflate_compress(input, &mut out[start..]) {
            Ok(n) => {
                out.truncate(start + n);
                Ok(true)
            }
            Err(_) => {
                out.truncate(start);
                Ok(false)
            }
        }
    }

    fn compress_chunk(&mut self, dict: &[u8], input: &[u8], last: bool, out: &mut Vec<u8>) -> Result<()> {
        if self.chunked.is_none() {
            self.chunked = Some(ZStream::new(self.level.min(9))?);
        }
        self.chunked.as_mut().expect("stream").chunk(dict, input, last, out)
    }
}

/// Raw-deflate zlib-rs stream reused across chunks.
struct ZStream {
    strm: Box<z::z_stream>,
}

// The stream owns its state exclusively and is only used from one thread at a time.
unsafe impl Send for ZStream {}

fn zerr(what: &str, code: i32) -> Error {
    Error::msg(format!("deflate: {what} failed ({code})"))
}

impl ZStream {
    fn new(level: u32) -> Result<Self> {
        let mut strm = Box::new(z::z_stream::default());
        // SAFETY: `strm` is a valid, default-initialized stream; window bits -15 select raw deflate.
        let ret = unsafe {
            z::deflateInit2_(
                &mut *strm,
                level as i32,
                z::Z_DEFLATED,
                -15,
                8,
                z::Z_DEFAULT_STRATEGY,
                z::zlibVersion(),
                std::mem::size_of::<z::z_stream>() as i32,
            )
        };
        if ret != z::Z_OK {
            return Err(zerr("init", ret));
        }
        Ok(Self { strm })
    }

    fn chunk(&mut self, dict: &[u8], input: &[u8], last: bool, out: &mut Vec<u8>) -> Result<()> {
        let s = &mut *self.strm;
        // SAFETY: the stream was initialized by deflateInit2_; input/output pointers stay valid for
        // the duration of each deflate call and lengths fit in u32 (chunks are far below 4 GiB).
        unsafe {
            let ret = z::deflateReset(s);
            if ret != z::Z_OK {
                return Err(zerr("reset", ret));
            }
            if !dict.is_empty() {
                let ret = z::deflateSetDictionary(s, dict.as_ptr(), dict.len() as u32);
                if ret != z::Z_OK {
                    return Err(zerr("set dictionary", ret));
                }
            }
            let flush = if last { z::Z_FINISH } else { z::Z_SYNC_FLUSH };
            s.next_in = input.as_ptr();
            s.avail_in = u32::try_from(input.len()).map_err(|_| Error::msg("deflate: chunk too large"))?;
            loop {
                if out.capacity() - out.len() < 1024 {
                    out.reserve((input.len() / 4).max(64 << 10));
                }
                let spare = out.spare_capacity_mut();
                let room = spare.len().min(u32::MAX as usize) as u32;
                s.next_out = spare.as_mut_ptr().cast::<u8>();
                s.avail_out = room;
                let ret = z::deflate(s, flush);
                let produced = (room - s.avail_out) as usize;
                out.set_len(out.len() + produced);
                match ret {
                    z::Z_STREAM_END => break,
                    z::Z_OK | z::Z_BUF_ERROR => {
                        if !last && s.avail_in == 0 && s.avail_out != 0 {
                            break;
                        }
                        if ret == z::Z_BUF_ERROR && produced == 0 && s.avail_out != 0 {
                            return Err(zerr("deflate", ret));
                        }
                    }
                    other => return Err(zerr("deflate", other)),
                }
            }
            s.next_in = std::ptr::null();
            s.next_out = std::ptr::null_mut();
        }
        Ok(())
    }
}

impl Drop for ZStream {
    fn drop(&mut self) {
        // SAFETY: the stream was initialized by deflateInit2_ and is ended exactly once.
        unsafe {
            z::deflateEnd(&mut *self.strm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample(n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        let mut x: u32 = 12345;
        while v.len() < n {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            let word = ["alpha ", "beta ", "gamma ", "delta ", "epsilon\n"][(x >> 16) as usize % 5];
            v.extend_from_slice(word.as_bytes());
            if x % 7 == 0 {
                v.push((x >> 8) as u8);
            }
        }
        v.truncate(n);
        v
    }

    fn inflate(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn whole_buffer_roundtrip_and_store_fallback() {
        let mut enc = Deflate.encoder(6).unwrap();
        let data = sample(100_000);
        let mut out = Vec::new();
        assert!(enc.compress(&data, &mut out).unwrap());
        assert!(out.len() < data.len());
        assert_eq!(inflate(&out), data);
        assert_eq!(Deflate.decode_whole(&out, data.len()).unwrap().unwrap(), data);
        assert!(Deflate.decode_whole(&out, data.len() - 1).unwrap().is_err());
        assert!(Deflate.decode_whole(&out, data.len() + 1).unwrap().is_err());

        let mut noise = vec![0u8; 4096];
        let mut x: u64 = 0x9e3779b97f4a7c15;
        for b in noise.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        let mut out2 = Vec::new();
        assert!(!enc.compress(&noise, &mut out2).unwrap());
        assert!(out2.is_empty());
    }

    #[test]
    fn chunked_stream_is_one_valid_deflate_stream() {
        let data = sample(1_000_000);
        for (chunk, level) in [(65_536usize, 1u32), (100_000, 6), (300_000, 9)] {
            let mut enc = Deflate.encoder(level).unwrap();
            let mut out = Vec::new();
            let mut off = 0;
            while off < data.len() {
                let end = (off + chunk).min(data.len());
                let dict = &data[off.saturating_sub(32 << 10)..off];
                enc.compress_chunk(dict, &data[off..end], end == data.len(), &mut out).unwrap();
                off = end;
            }
            assert_eq!(inflate(&out), data);
            assert_eq!(Deflate.decode_whole(&out, data.len()).unwrap().unwrap(), data);
        }
    }

    #[test]
    fn single_and_empty_chunks() {
        let mut enc = Deflate.encoder(6).unwrap();
        let mut out = Vec::new();
        enc.compress_chunk(&[], b"hello", true, &mut out).unwrap();
        assert_eq!(inflate(&out), b"hello");
        let mut out = Vec::new();
        enc.compress_chunk(&[], b"", true, &mut out).unwrap();
        assert_eq!(inflate(&out), b"");
    }
}
