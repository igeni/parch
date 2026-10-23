//! Entry name encodings: UTF-8 (general purpose bit 11), OEM code pages, other legacy charsets.

use oem_cp::code_table::{DECODING_TABLE_CP_MAP, ENCODING_TABLE_CP_MAP};
use parch_core::{Error, Result};

use crate::spec::FLAG_UTF8;

#[derive(Clone, Copy)]
pub enum Charset {
    Oem(u16),
    Other(&'static encoding_rs::Encoding),
}

/// Parses `utf8`, `cp866`, `866`, `ibm866`, `windows-1251`, `koi8-r`, ... (`None` = UTF-8).
pub fn parse_charset(s: &str) -> Result<Option<Charset>> {
    let l = s.trim().to_ascii_lowercase();
    if l == "utf8" || l == "utf-8" {
        return Ok(None);
    }
    let digits = l.trim_start_matches("cp").trim_start_matches("ibm").trim_start_matches("oem");
    if let Ok(cp) = digits.parse::<u16>() {
        if DECODING_TABLE_CP_MAP.get(&cp).is_some() {
            return Ok(Some(Charset::Oem(cp)));
        }
    }
    encoding_rs::Encoding::for_label(l.as_bytes())
        .map(|e| Some(Charset::Other(e)))
        .ok_or_else(|| Error::msg(format!("unknown charset: {s}")))
}

impl Charset {
    pub fn decode(&self, raw: &[u8]) -> String {
        match self {
            Charset::Oem(cp) => match DECODING_TABLE_CP_MAP.get(cp) {
                Some(t) => t.decode_string_lossy(raw),
                None => String::from_utf8_lossy(raw).into_owned(),
            },
            Charset::Other(e) => e.decode_without_bom_handling(raw).0.into_owned(),
        }
    }

    pub fn encode(&self, s: &str) -> Option<Vec<u8>> {
        match self {
            Charset::Oem(cp) => oem_cp::encode_string_checked(s, ENCODING_TABLE_CP_MAP.get(cp)?),
            Charset::Other(e) => {
                let (b, _, errors) = e.encode(s);
                (!errors).then(|| b.into_owned())
            }
        }
    }
}

/// Decodes a stored name: UTF-8 flag → Unicode Path field (CRC-checked) → explicit charset →
/// valid UTF-8 → CP437.
pub fn decode(raw: &[u8], flags: u16, unicode_path: Option<&(u32, String)>, charset: Option<&Charset>) -> String {
    if flags & FLAG_UTF8 != 0 {
        return String::from_utf8_lossy(raw).into_owned();
    }
    if let Some((crc, name)) = unicode_path {
        if *crc == crc32fast::hash(raw) {
            return name.clone();
        }
    }
    if let Some(cs) = charset {
        return cs.decode(raw);
    }
    if raw.is_ascii() {
        return String::from_utf8_lossy(raw).into_owned();
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => Charset::Oem(437).decode(raw),
    }
}

pub struct Encoded {
    pub raw: Vec<u8>,
    pub utf8_flag: bool,
    /// UTF-8 name for a Unicode Path extra field when `raw` uses a legacy charset.
    pub unicode_path: Option<String>,
}

pub fn encode(name: Vec<u8>, charset: Option<&Charset>) -> Encoded {
    if name.is_ascii() {
        return Encoded { raw: name, utf8_flag: false, unicode_path: None };
    }
    let Ok(s) = std::str::from_utf8(&name) else {
        return Encoded { raw: name, utf8_flag: false, unicode_path: None };
    };
    if let Some(raw) = charset.and_then(|cs| cs.encode(s)) {
        return Encoded { raw, utf8_flag: false, unicode_path: Some(s.to_string()) };
    }
    Encoded { raw: name, utf8_flag: true, unicode_path: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_order() {
        let cp866 = parse_charset("cp866").unwrap().unwrap();
        let raw = cp866.encode("Привет").unwrap();
        assert_eq!(decode(&raw, 0, None, Some(&cp866)), "Привет");
        assert_eq!(decode("Привет".as_bytes(), FLAG_UTF8, None, None), "Привет");
        assert_eq!(decode("Привет".as_bytes(), 0, None, None), "Привет");
        let up = (crc32fast::hash(&raw), "Привет".to_string());
        assert_eq!(decode(&raw, 0, Some(&up), None), "Привет");
        let bad = (0, "x".to_string());
        assert_eq!(decode(&[0x80], 0, Some(&bad), None), "Ç");
        assert_eq!(decode(b"plain", 0, None, None), "plain");
    }

    #[test]
    fn encoding_choices() {
        let e = encode(b"ascii/name".to_vec(), None);
        assert!(!e.utf8_flag && e.unicode_path.is_none());
        let e = encode("файл".as_bytes().to_vec(), None);
        assert!(e.utf8_flag);
        let cs = parse_charset("866").unwrap();
        let e = encode("файл".as_bytes().to_vec(), cs.as_ref());
        assert!(!e.utf8_flag);
        assert_eq!(e.unicode_path.as_deref(), Some("файл"));
        let e = encode("日本".as_bytes().to_vec(), cs.as_ref());
        assert!(e.utf8_flag);
        assert!(parse_charset("utf-8").unwrap().is_none());
        assert!(parse_charset("windows-1251").unwrap().is_some());
        assert!(parse_charset("nonsense").is_err());
    }
}
