// SPDX-License-Identifier: MIT
//! C-style path quoting as used in git's line-oriented input and output
//! (`"a\tb"`, octal escapes for other bytes).

/// Unquote a C-style quoted string. The input must start with `"`; the
/// closing quote must be the last byte. Returns `None` on bad quoting.
pub fn c_unquote(s: &[u8]) -> Option<Vec<u8>> {
    let (rest, tail) = c_unquote_prefix(s)?;
    tail.is_empty().then_some(rest)
}

/// Unquote the quoted string at the start of `s`; returns it and the bytes
/// after the closing quote.
pub fn c_unquote_prefix(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut it = s.strip_prefix(b"\"")?.iter().enumerate();
    let body = &s[1..];
    let mut out = Vec::new();
    while let Some((i, &c)) = it.next() {
        match c {
            b'"' => return Some((out, &body[i + 1..])),
            b'\\' => {
                let (_, &e) = it.next()?;
                let b = match e {
                    b'a' => 7,
                    b'b' => 8,
                    b'f' => 12,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 11,
                    b'\\' | b'"' => e,
                    b'0'..=b'3' => {
                        let (_, &d2) = it.next()?;
                        let (_, &d3) = it.next()?;
                        if !(b'0'..=b'7').contains(&d2) || !(b'0'..=b'7').contains(&d3) {
                            return None;
                        }
                        ((e - b'0') << 6) | ((d2 - b'0') << 3) | (d3 - b'0')
                    }
                    _ => return None,
                };
                out.push(b);
            }
            _ => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote() {
        assert_eq!(c_unquote(b"\"a\\tb\"").unwrap(), b"a\tb");
        assert_eq!(c_unquote(b"\"\\303\\244\"").unwrap(), "ä".as_bytes());
        assert!(c_unquote(b"\"bad").is_none());
        assert!(c_unquote(b"\"a\"x").is_none());
        assert!(c_unquote(b"\"\\q\"").is_none());
    }
}
