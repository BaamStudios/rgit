// SPDX-License-Identifier: MIT
//! MIME decoding: encoded words in headers (RFC 2047) and the
//! quoted-printable and base64 transfer encodings (RFC 2045).
//!
//! Behaviour checked against `git mailinfo`:
//!
//! - Encoded words are decoded wherever they appear, also inside other
//!   text (`x=?utf-8?q?a?=y` is `xay`); whitespace between two encoded
//!   words is dropped.
//! - An encoding other than `Q`/`B` is an error, as is an unknown charset
//!   (which is reported); `=?` without a complete word is kept as text.
//! - Invalid `=XX` escapes are kept literally, base64 ignores characters
//!   outside its alphabet and drops an incomplete last group.

use crate::charset;

/// Why a header could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// An encoded word with an encoding other than `Q` or `B`.
    Encoding,
    /// An encoded word in a charset that cannot be converted (its name).
    Charset(String),
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decodes quoted-printable text; with `underscore` (the `Q` encoding of
/// encoded words) `_` is a space. A `=` at the end of the input is a soft
/// line break and is dropped.
pub(crate) fn decode_qp(data: &[u8], underscore: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if c == b'=' {
            let rest = &data[i + 1..];
            if rest.is_empty() || rest == b"\n" || rest == b"\r\n" {
                // Soft line break.
                break;
            }
            if rest.len() >= 2 {
                if let (Some(h), Some(l)) = (hex(rest[0]), hex(rest[1])) {
                    out.push(h << 4 | l);
                    i += 3;
                    continue;
                }
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(if underscore && c == b'_' { b' ' } else { c });
        i += 1;
    }
    out
}

fn b64_value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes base64, ignoring characters outside the alphabet; `=` ends a
/// group early.
pub(crate) fn decode_base64(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut n = 0;
    let flush = |acc: u32, n: i32, out: &mut Vec<u8>| match n {
        2 => out.push((acc >> 4) as u8),
        3 => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
        _ => {}
    };
    for &c in data {
        if c == b'=' {
            flush(acc, n, &mut out);
            acc = 0;
            n = 0;
            continue;
        }
        let Some(v) = b64_value(c) else { continue };
        acc = acc << 6 | v;
        n += 1;
        if n == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            n = 0;
        }
    }
    flush(acc, n, &mut out);
    out
}

/// One encoded word starting at `data[0..]` (which begins with `=?`):
/// (charset, encoding, text, length).
fn encoded_word(data: &[u8]) -> Option<(&[u8], u8, &[u8], usize)> {
    let rest = &data[2..];
    let q1 = rest.iter().position(|&b| b == b'?')?;
    let charset = &rest[..q1];
    let after = &rest[q1 + 1..];
    if after.len() < 2 || after[1] != b'?' {
        return None;
    }
    let enc = after[0];
    let text_start = &after[2..];
    let end = text_start.windows(2).position(|w| w == b"?=")?;
    let text = &text_start[..end];
    let len = 2 + q1 + 1 + 2 + end + 2;
    Some((charset, enc, text, len))
}

/// Decodes the encoded words of a header value and converts them to
/// `target` (`None`: no conversion).
pub fn decode_header(value: &[u8], target: Option<&str>) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    // Where the text after the last encoded word starts in `out`.
    let mut after_word: Option<usize> = None;
    while i < value.len() {
        if value[i..].starts_with(b"=?") {
            if let Some((cs, enc, text, len)) = encoded_word(&value[i..]) {
                if let Some(start) = after_word {
                    if out[start..].iter().all(u8::is_ascii_whitespace) {
                        out.truncate(start);
                    }
                }
                let decoded = match enc {
                    b'q' | b'Q' => decode_qp(text, true),
                    b'b' | b'B' => decode_base64(text),
                    _ => return Err(DecodeError::Encoding),
                };
                let cs = String::from_utf8_lossy(cs).into_owned();
                let converted = match target {
                    Some(t) => charset::convert(&decoded, &cs, t)
                        .ok_or_else(|| DecodeError::Charset(cs.clone()))?,
                    None => decoded,
                };
                out.extend_from_slice(&converted);
                after_word = Some(out.len());
                i += len;
                continue;
            }
        }
        out.push(value[i]);
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> String {
        String::from_utf8(decode_header(s.as_bytes(), Some("UTF-8")).unwrap()).unwrap()
    }

    #[test]
    fn words_and_whitespace() {
        assert_eq!(d("=?ISO8859-1?Q?a?= =?ISO8859-2?Q?_b?="), "a b");
        assert_eq!(d("(=?ISO8859-1?Q?a?=  =?ISO8859-1?Q?b?=)"), "(ab)");
        assert_eq!(d("(=?ISO8859-1?Q?a?= b)"), "(a b)");
        assert_eq!(d("x=?utf-8?q?a?=y"), "xay");
        assert_eq!(d("=?utf-8?b?YWJj?="), "abc");
        assert_eq!(d("=?utf-8?q?a=3?="), "a=3");
        assert_eq!(d("=?UTF-8?q?J=C3=B6rg?= <j@x>"), "Jörg <j@x>");
        assert_eq!(d("=? not a word"), "=? not a word");
    }

    #[test]
    fn errors() {
        assert_eq!(
            decode_header(b"=?utf-8?x?ab?=", Some("UTF-8")),
            Err(DecodeError::Encoding)
        );
        assert_eq!(
            decode_header(b"=?bogus?q?a?=", Some("UTF-8")),
            Err(DecodeError::Charset("bogus".into()))
        );
        assert!(decode_header(b"=?bogus?q?a?=", None).is_ok());
    }

    #[test]
    fn transfer_encodings() {
        assert_eq!(decode_qp(b"soft=\n", false), b"soft");
        assert_eq!(decode_qp(b"a=3D=20b=\r\n", false), b"a= b");
        assert_eq!(decode_qp(b"x=zz\n", false), b"x=zz\n");
        assert_eq!(decode_base64(b"Ym9k eQ==\n"), b"body");
        assert_eq!(decode_base64(b"Ym9keQ"), b"body");
    }
}
