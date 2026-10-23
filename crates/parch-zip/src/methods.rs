//! Compression method ids and their mapping to codecs.

use parch_core::codec::CodecId;

pub const STORE: u16 = 0;
pub const DEFLATE: u16 = 8;
pub const DEFLATE64: u16 = 9;
pub const BZIP2: u16 = 12;
pub const LZMA: u16 = 14;
pub const ZSTD_OLD: u16 = 20;
pub const ZSTD: u16 = 93;
pub const XZ: u16 = 95;
pub const PPMD: u16 = 98;
pub const AES: u16 = 99;

pub fn codec(method: u16) -> Option<CodecId> {
    Some(match method {
        STORE => CodecId::Store,
        DEFLATE => CodecId::Deflate,
        DEFLATE64 => CodecId::Deflate64,
        BZIP2 => CodecId::Bzip2,
        LZMA => CodecId::Lzma,
        ZSTD | ZSTD_OLD => CodecId::Zstd,
        XZ => CodecId::Xz,
        PPMD => CodecId::Ppmd,
        _ => return None,
    })
}

pub fn method(codec: CodecId) -> u16 {
    match codec {
        CodecId::Store => STORE,
        CodecId::Deflate => DEFLATE,
        CodecId::Deflate64 => DEFLATE64,
        CodecId::Bzip2 => BZIP2,
        CodecId::Lzma => LZMA,
        CodecId::Zstd => ZSTD,
        CodecId::Xz => XZ,
        CodecId::Ppmd => PPMD,
    }
}

/// Minimum "version needed to extract" for a method.
pub fn version_needed(method: u16) -> u16 {
    match method {
        STORE => 10,
        DEFLATE => 20,
        DEFLATE64 => 21,
        BZIP2 => 46,
        _ => 63,
    }
}

/// Method label in the style of `unzip -v`.
pub fn label(method: u16, flags: u16) -> String {
    match method {
        STORE => "Stored".into(),
        DEFLATE => format!("Defl:{}", ['N', 'X', 'F', 'S'][((flags >> 1) & 3) as usize]),
        DEFLATE64 => "Def64".into(),
        BZIP2 => "BZip2".into(),
        LZMA => "LZMA".into(),
        ZSTD | ZSTD_OLD => "Zstd".into(),
        XZ => "XZ".into(),
        PPMD => "PPMd".into(),
        AES => "AES".into(),
        m => format!("Unk:{m:03}"),
    }
}
