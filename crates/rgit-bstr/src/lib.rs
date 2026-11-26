// SPDX-License-Identifier: MIT
//! Byte strings that are not UTF-8, carried losslessly in `String`.
//!
//! git treats reference names, command-line arguments and many other
//! strings as plain bytes. rgit's APIs use `String`/`&str` for them; bytes
//! that do not form valid UTF-8 are kept as the private use characters
//! U+10FF00 + byte ("escapes"). A valid UTF-8 character that itself lies
//! in that range is escaped byte by byte too, so the mapping between byte
//! strings and escaped strings is a bijection:
//!
//! * [`from_bytes`] / [`from_vec`] / [`from_os`]: raw bytes to a string,
//! * [`to_bytes`] / [`to_os`]: back to the raw bytes, at the boundaries
//!   where they are stored (file names, `packed-refs`, reftable, the
//!   wire) or printed,
//! * [`cmp`]: the byte order of the raw strings (git sorts names by bytes).
//!
//! Strings without escapes are returned unchanged (borrowed), so the
//! conversions cost nothing for ordinary UTF-8 names.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};

/// The first escape character; byte `b` is `BASE + b`.
const BASE: u32 = 0x10ff00;

/// Whether `c` is an escape character (U+10FF00..=U+10FFFF).
pub fn is_escape(c: char) -> bool {
    u32::from(c) >= BASE
}

/// Whether `s` contains escape characters, i.e. its raw bytes differ from
/// its UTF-8 encoding.
pub fn has_escapes(s: &str) -> bool {
    // U+10FF00..=U+10FFFF encode as F4 8F BC..=BF xx.
    s.as_bytes()
        .windows(3)
        .any(|w| w[0] == 0xf4 && w[1] == 0x8f && w[2] >= 0xbc)
}

fn escape(b: u8) -> char {
    char::from_u32(BASE + u32::from(b)).unwrap_or('\u{fffd}')
}

fn push_escaped(out: &mut String, valid: &str) {
    for c in valid.chars() {
        if is_escape(c) {
            let mut buf = [0; 4];
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                out.push(escape(b));
            }
        } else {
            out.push(c);
        }
    }
}

/// `raw` as a string: valid UTF-8 as it is, other bytes escaped.
pub fn from_bytes(raw: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(raw) {
        if !has_escapes(s) {
            return Cow::Borrowed(s);
        }
    }
    let mut out = String::with_capacity(raw.len() + 8);
    for chunk in raw.utf8_chunks() {
        push_escaped(&mut out, chunk.valid());
        for &b in chunk.invalid() {
            out.push(escape(b));
        }
    }
    Cow::Owned(out)
}

/// [`from_bytes`] for an owned buffer.
pub fn from_vec(raw: Vec<u8>) -> String {
    match String::from_utf8(raw) {
        Ok(s) if !has_escapes(&s) => s,
        Ok(s) => from_bytes(s.as_bytes()).into_owned(),
        Err(e) => from_bytes(e.as_bytes()).into_owned(),
    }
}

/// The raw bytes of a string made by [`from_bytes`] (or any `&str`: a
/// string without escapes is its UTF-8 encoding).
pub fn to_bytes(s: &str) -> Cow<'_, [u8]> {
    if !has_escapes(s) {
        return Cow::Borrowed(s.as_bytes());
    }
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        if is_escape(c) {
            out.push((u32::from(c) - BASE) as u8);
        } else {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    Cow::Owned(out)
}

/// A file name or argument as a string (its bytes on Unix; elsewhere the
/// lossy conversion).
pub fn from_os(os: &OsStr) -> Cow<'_, str> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        from_bytes(os.as_bytes())
    }
    #[cfg(not(unix))]
    {
        os.to_string_lossy()
    }
}

/// [`from_os`] for an owned value.
pub fn from_os_string(os: OsString) -> String {
    match os.into_string() {
        Ok(s) if !has_escapes(&s) => s,
        Ok(s) => from_bytes(s.as_bytes()).into_owned(),
        Err(os) => from_os(&os).into_owned(),
    }
}

/// The file name for `s`: its raw bytes on Unix. Elsewhere the escapes
/// stay as they are (such names cannot be spelled there).
pub fn to_os(s: &str) -> Cow<'_, OsStr> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        match to_bytes(s) {
            Cow::Borrowed(b) => Cow::Borrowed(OsStr::from_bytes(b)),
            Cow::Owned(v) => {
                use std::os::unix::ffi::OsStringExt;
                Cow::Owned(OsString::from_vec(v))
            }
        }
    }
    #[cfg(not(unix))]
    {
        Cow::Borrowed(OsStr::new(s))
    }
}

/// Compares the raw bytes of `a` and `b`.
pub fn cmp(a: &str, b: &str) -> Ordering {
    if !has_escapes(a) && !has_escapes(b) {
        return a.cmp(b);
    }
    to_bytes(a).cmp(&to_bytes(b))
}

/// `s` for messages and output that must stay text: escapes replaced by
/// U+FFFD, as `String::from_utf8_lossy` shows the raw bytes.
pub fn lossy(s: &str) -> Cow<'_, str> {
    match to_bytes(s) {
        Cow::Borrowed(_) => Cow::Borrowed(s),
        Cow::Owned(v) => Cow::Owned(String::from_utf8_lossy(&v).into_owned()),
    }
}

/// A writer that passes text through and turns escapes back into their
/// raw bytes ([`to_bytes`] on the stream).
///
/// For output that consists of names and fixed text only (`show-ref`,
/// `check-ref-format`, ...): wrapping its writer prints every name as the
/// bytes git would print. Not for object content, where a real U+10FFxx
/// character must stay as it is.
pub struct Unescape<W: std::io::Write> {
    inner: W,
    /// The start of an escape split between two writes.
    pending: Vec<u8>,
}

impl<W: std::io::Write> Unescape<W> {
    /// Wraps `inner`.
    pub fn new(inner: W) -> Self {
        Unescape {
            inner,
            pending: Vec::new(),
        }
    }

    /// The wrapped writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

impl<W: std::io::Write> std::io::Write for Unescape<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(buf);
        // Keep an incomplete escape (F4, F4 8F, F4 8F Bx) at the end for
        // the next write.
        let mut keep = 0;
        for k in 1..=3.min(data.len()) {
            let tail = &data[data.len() - k..];
            let prefix_of_escape = tail[0] == 0xf4
                && tail.get(1).is_none_or(|&b| b == 0x8f)
                && tail.get(2).is_none_or(|&b| b >= 0xbc);
            if prefix_of_escape {
                keep = k;
                break;
            }
        }
        let (now, later) = data.split_at(data.len() - keep);
        let out = unescape_bytes(now);
        self.inner.write_all(&out)?;
        self.pending = later.to_vec();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.inner.write_all(&rest)?;
        }
        self.inner.flush()
    }
}

impl<W: std::io::Write> Drop for Unescape<W> {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            let _ = self.inner.write_all(&rest);
        }
    }
}

/// Replaces every escape in `data` (UTF-8 of U+10FF00..=U+10FFFF) by its
/// byte; everything else is copied.
fn unescape_bytes(data: &[u8]) -> Cow<'_, [u8]> {
    let is_escape_at = |i: usize| {
        data.len() >= i + 4 && data[i] == 0xf4 && data[i + 1] == 0x8f && data[i + 2] >= 0xbc
    };
    if !(0..data.len()).any(is_escape_at) {
        return Cow::Borrowed(data);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if is_escape_at(i) {
            // F4 8F (BC | hi) (80 | lo): byte = (hi << 6) | lo.
            out.push(((data[i + 2] & 0x03) << 6) | (data[i + 3] & 0x3f));
            i += 4;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unescape_writer() {
        use std::io::Write;
        let s = from_bytes(b"a\xe9b\x80").into_owned();
        let mut w = Unescape::new(Vec::new());
        // Split inside the escape.
        let b = s.as_bytes();
        w.write_all(&b[..3]).unwrap();
        w.write_all(&b[3..]).unwrap();
        w.flush().unwrap();
        assert_eq!(w.get_mut().as_slice(), b"a\xe9b\x80");
        let mut w = Unescape::new(Vec::new());
        w.write_all(b"x\xf4").unwrap();
        drop(w);
    }

    use super::*;

    #[test]
    fn plain_strings_are_borrowed() {
        assert!(matches!(from_bytes(b"refs/heads/main"), Cow::Borrowed(_)));
        assert!(matches!(
            from_bytes("caf\u{e9}".as_bytes()),
            Cow::Borrowed(_)
        ));
        assert!(matches!(to_bytes("refs/heads/caf\u{e9}"), Cow::Borrowed(_)));
    }

    #[test]
    fn round_trip() {
        let cases: &[&[u8]] = &[
            b"caf\xe9",
            b"\xc3\xa9 \xa4\xa2 \xff\xfe",
            // A valid U+10FFE9 is escaped byte by byte.
            b"x\xf4\x8f\xbf\xa9y",
            b"\xf4\x8f\xbf",
            b"",
        ];
        for &raw in cases {
            let s = from_bytes(raw);
            assert_eq!(to_bytes(&s).as_ref(), raw, "{raw:?}");
            assert_eq!(from_vec(raw.to_vec()), s);
        }
        let s = from_bytes(b"caf\xe9");
        assert_eq!(s, "caf\u{10ffe9}");
        assert!(has_escapes(&s));
        assert_eq!(lossy(&s), "caf\u{fffd}");
    }

    #[test]
    fn byte_order() {
        let raw = from_bytes(b"a\x80");
        // U+00E9 is C3 A9, after the raw byte 0x80.
        assert_eq!(cmp(&raw, "a\u{e9}"), Ordering::Less);
        assert_eq!(cmp("a", "b"), Ordering::Less);
        assert_eq!(cmp(&raw, &raw), Ordering::Equal);
    }

    #[cfg(unix)]
    #[test]
    fn os_strings() {
        use std::os::unix::ffi::OsStringExt;
        let raw = b"caf\xe9".to_vec();
        let s = from_os_string(OsString::from_vec(raw.clone()));
        assert_eq!(s, "caf\u{10ffe9}");
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(to_os(&s).as_bytes(), raw.as_slice());
    }
}
