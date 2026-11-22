// SPDX-License-Identifier: MIT
//! Re-coding text between character sets, the way mail metadata and
//! message bodies are converted to the commit encoding.
//!
//! Charsets are looked up with `encoding_rs` (WHATWG labels such as
//! `ISO8859-1`, `iso-2022-jp`, `koi8-r`). ISO-8859-1 and its aliases map
//! every byte to the code point of the same value (the WHATWG label maps
//! them to windows-1252, which differs in 0x80-0x9f); US-ASCII is passed
//! through.

use encoding_rs::Encoding;

enum Kind {
    Utf8,
    Latin1,
    Ascii,
    Other(&'static Encoding),
}

fn kind(name: &str) -> Option<Kind> {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "utf-8" | "utf8" => Some(Kind::Utf8),
        "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "iso88591" | "latin1" | "latin-1" | "l1"
        | "iso-ir-100" | "cp819" | "ibm819" => Some(Kind::Latin1),
        "us-ascii" | "ascii" | "ansi_x3.4-1968" | "us" => Some(Kind::Ascii),
        _ => Encoding::for_label(lower.as_bytes()).map(|e| {
            if e == encoding_rs::UTF_8 {
                Kind::Utf8
            } else {
                Kind::Other(e)
            }
        }),
    }
}

/// Whether `name` is a charset this module can convert from or to.
pub fn is_known(name: &str) -> bool {
    kind(name).is_some()
}

/// Whether two charset names mean the same encoding.
pub fn same(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    match (kind(a), kind(b)) {
        (Some(Kind::Utf8), Some(Kind::Utf8)) => true,
        (Some(Kind::Latin1), Some(Kind::Latin1)) => true,
        (Some(Kind::Ascii), Some(Kind::Ascii)) => true,
        (Some(Kind::Other(x)), Some(Kind::Other(y))) => x == y,
        _ => false,
    }
}

fn to_unicode(data: &[u8], from: &Kind) -> Option<String> {
    match from {
        Kind::Utf8 | Kind::Ascii => String::from_utf8(data.to_vec()).ok(),
        Kind::Latin1 => Some(data.iter().map(|&b| char::from(b)).collect()),
        Kind::Other(e) => {
            let (text, malformed) = e.decode_without_bom_handling(data);
            (!malformed).then(|| text.into_owned())
        }
    }
}

fn from_unicode(text: &str, to: &Kind) -> Option<Vec<u8>> {
    match to {
        Kind::Utf8 => Some(text.as_bytes().to_vec()),
        Kind::Ascii => text.is_ascii().then(|| text.as_bytes().to_vec()),
        Kind::Latin1 => text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect(),
        Kind::Other(e) => {
            let (bytes, _, unmappable) = e.encode(text);
            (!unmappable).then(|| bytes.into_owned())
        }
    }
}

/// Converts `data` from the charset `from` to `to`; `None` when either
/// charset is unknown or the text cannot be converted.
pub fn convert(data: &[u8], from: &str, to: &str) -> Option<Vec<u8>> {
    let f = kind(from)?;
    let t = kind(to)?;
    if same(from, to) {
        return Some(data.to_vec());
    }
    if matches!(f, Kind::Ascii) && matches!(t, Kind::Utf8) {
        return Some(data.to_vec());
    }
    from_unicode(&to_unicode(data, &f)?, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_to_utf8() {
        assert_eq!(
            convert(b"caf\xe9", "ISO8859-1", "UTF-8").unwrap(),
            "café".as_bytes()
        );
        assert_eq!(
            convert(b"\x80", "latin1", "utf-8").unwrap(),
            "\u{80}".as_bytes()
        );
    }

    #[test]
    fn utf8_to_latin1() {
        assert_eq!(
            convert("café".as_bytes(), "utf-8", "iso-8859-1").unwrap(),
            b"caf\xe9"
        );
    }

    #[test]
    fn unknown_charset() {
        assert!(convert(b"x", "no-such-charset", "UTF-8").is_none());
        assert!(!is_known("utf-8*en"));
    }

    #[test]
    fn same_charset_passes_bytes_through() {
        assert_eq!(convert(b"\xff", "utf-8", "UTF8").unwrap(), b"\xff");
    }
}
