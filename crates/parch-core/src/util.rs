/// Parses sizes like `4096`, `64K`, `256M`, `1G` (binary multiples, optional `B`/`iB`).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let up = s.trim().to_ascii_uppercase();
    let t = up.strip_suffix("IB").or_else(|| up.strip_suffix('B')).unwrap_or(&up);
    let (num, mul) = match t.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => {
            let mul: u64 = match c {
                'K' => 1 << 10,
                'M' => 1 << 20,
                'G' => 1 << 30,
                'T' => 1 << 40,
                _ => return Err(format!("invalid size: {s}")),
            };
            (&t[..i], mul)
        }
        _ => (t, 1),
    };
    let n: u64 = num.trim().parse().map_err(|_| format!("invalid size: {s}"))?;
    n.checked_mul(mul).ok_or_else(|| format!("size too large: {s}"))
}

/// Lowercased extension of the last path component of an archive name.
pub fn extension(name: &[u8]) -> Option<String> {
    let base = name.rsplit(|&b| b == b'/').next()?;
    let dot = base.iter().rposition(|&b| b == b'.')?;
    if dot == 0 || dot + 1 == base.len() {
        return None;
    }
    std::str::from_utf8(&base[dot + 1..]).ok().map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("4096"), Ok(4096));
        assert_eq!(parse_size("64K"), Ok(65536));
        assert_eq!(parse_size("256M"), Ok(256 << 20));
        assert_eq!(parse_size("1GiB"), Ok(1 << 30));
        assert_eq!(parse_size("2mb"), Ok(2 << 20));
        assert!(parse_size("12Q").is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn extensions() {
        assert_eq!(extension(b"a/b/Photo.JPG").as_deref(), Some("jpg"));
        assert_eq!(extension(b"a.d/file"), None);
        assert_eq!(extension(b".bashrc"), None);
        assert_eq!(extension(b"x."), None);
    }
}
