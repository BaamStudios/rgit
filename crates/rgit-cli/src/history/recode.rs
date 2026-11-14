// SPDX-License-Identifier: MIT
//! Re-encoding commits for output (Documentation/git-log.adoc,
//! "DISCUSSION"; `i18n.logOutputEncoding`, `i18n.commitEncoding`).
//!
//! A commit whose `encoding` header names another encoding than the output
//! encoding is converted before it is shown. Only UTF-8 and ISO-8859-1
//! are converted so far (the full set is #182); other commits are shown
//! as stored, as git does when a conversion fails.

use rgit_config::ConfigSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    Utf8,
    Latin1,
    Other,
}

fn charset(name: &[u8]) -> Charset {
    let norm: Vec<u8> = name
        .iter()
        .filter(|&&b| b != b'-' && b != b'_')
        .map(u8::to_ascii_lowercase)
        .collect();
    match &norm[..] {
        b"utf8" => Charset::Utf8,
        b"iso88591" | b"latin1" | b"l1" => Charset::Latin1,
        _ => Charset::Other,
    }
}

/// The encoding output is written in.
pub fn output_encoding(config: &ConfigSet) -> Vec<u8> {
    for key in ["i18n.logOutputEncoding", "i18n.commitEncoding"] {
        if let Ok(Some(v)) = config.string(key) {
            return v.to_vec();
        }
    }
    b"UTF-8".to_vec()
}

fn latin1_to_utf8(s: &[u8]) -> Vec<u8> {
    s.iter()
        .map(|&b| char::from(b))
        .collect::<String>()
        .into_bytes()
}

fn utf8_to_latin1(s: &[u8]) -> Option<Vec<u8>> {
    std::str::from_utf8(s)
        .ok()?
        .chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect()
}

/// The raw commit `raw` (stored in `encoding`, UTF-8 when `None`)
/// converted to `output`, or `None` when it stays as it is.
pub fn reencode(raw: &[u8], encoding: Option<&[u8]>, output: &[u8]) -> Option<Vec<u8>> {
    let from = charset(encoding.unwrap_or(b"UTF-8"));
    let to = charset(output);
    match (from, to) {
        (Charset::Latin1, Charset::Utf8) => Some(latin1_to_utf8(raw)),
        (Charset::Utf8, Charset::Latin1) => utf8_to_latin1(raw),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(
            reencode(b"a\xe4", Some(b"ISO8859-1"), b"utf-8").unwrap(),
            "aä".as_bytes()
        );
        assert_eq!(
            reencode("aä".as_bytes(), None, b"latin1").unwrap(),
            b"a\xe4"
        );
        assert_eq!(reencode(b"x", Some(b"UTF-8"), b"utf8"), None);
        assert_eq!(reencode("€".as_bytes(), None, b"ISO-8859-1"), None);
        assert_eq!(reencode(b"x", Some(b"SHIFT-JIS"), b"utf8"), None);
    }
}
