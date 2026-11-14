// SPDX-License-Identifier: MIT

//! The `ident` attribute (gitattributes(5)): `$Id$` in the blob becomes
//! `$Id: <blob id> $` in the work tree and back.
//!
//! Details determined with the `git` binary:
//!
//! - Check-in replaces every `$Id:` ... `$` that has no line feed in
//!   between with `$Id$`, and continues after the closing `$`.
//! - Checkout replaces `$Id$` and `$Id:` ... `$` (no line feed in between)
//!   with the expanded form, except a "foreign" ident: one whose text
//!   after `$Id:` starts with a space and has another whitespace character
//!   before its last character (`$Id: file.c,v 1.2 $` is kept).

const OPEN: &[u8] = b"$Id";

/// Where `$Id:` text starting at `start` (just after the colon) ends: the
/// index of the closing `$`, unless a line feed comes first.
fn closing(data: &[u8], start: usize) -> Option<usize> {
    let rest = &data[start..];
    let end = rest.iter().position(|&b| b == b'$' || b == b'\n')?;
    (rest[end] == b'$').then_some(start + end)
}

/// `data` with every `$Id: ... $` collapsed to `$Id$`; `None` when there
/// is nothing to collapse.
#[must_use]
pub fn collapse(data: &[u8]) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    let mut copied = 0;
    let mut i = 0;
    while let Some(off) = find(&data[i..], b"$Id:") {
        let colon = i + off + 3;
        match closing(data, colon + 1) {
            Some(close) => {
                let o = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
                o.extend_from_slice(&data[copied..colon]);
                o.push(b'$');
                copied = close + 1;
                i = close + 1;
            }
            None => i = colon + 1,
        }
    }
    let mut o = out?;
    o.extend_from_slice(&data[copied..]);
    Some(o)
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether the text between `$Id:` and `$` is someone else's keyword.
fn is_foreign(text: &[u8]) -> bool {
    if text.first() != Some(&b' ') {
        return false;
    }
    text.iter()
        .skip(1)
        .position(|&b| is_space(b))
        .is_some_and(|p| p + 2 < text.len())
}

/// `data` with `$Id$` and `$Id: ... $` expanded to `$Id: <hex> $`;
/// `None` when there is nothing to expand.
#[must_use]
pub fn expand(data: &[u8], hex: &str) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    let mut copied = 0;
    let mut i = 0;
    while let Some(off) = find(&data[i..], OPEN) {
        let start = i + off;
        let after = start + OPEN.len();
        let end = match data.get(after) {
            Some(b'$') => Some(after),
            Some(b':') => closing(data, after + 1).filter(|&c| !is_foreign(&data[after + 1..c])),
            _ => None,
        };
        let Some(end) = end else {
            i = after;
            continue;
        };
        let o = out.get_or_insert_with(|| Vec::with_capacity(data.len() + 60));
        o.extend_from_slice(&data[copied..start]);
        o.extend_from_slice(b"$Id: ");
        o.extend_from_slice(hex.as_bytes());
        o.extend_from_slice(b" $");
        copied = end + 1;
        i = end + 1;
    }
    let mut o = out?;
    o.extend_from_slice(&data[copied..]);
    Some(o)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses() {
        let c = |d: &[u8]| collapse(d).map(|v| String::from_utf8(v).unwrap());
        assert_eq!(c(b"$Id$ x"), None);
        assert_eq!(c(b"a $Id: foo $ b").as_deref(), Some("a $Id$ b"));
        assert_eq!(c(b"$Id:x\n$"), None);
        assert_eq!(c(b"$Id: a $Id: b $").as_deref(), Some("$Id$Id: b $"));
        assert_eq!(c(b"$Id: q $Id$ end").as_deref(), Some("$Id$Id$ end"));
        assert_eq!(c(b"$Id:$").as_deref(), Some("$Id$"));
    }

    #[test]
    fn expands() {
        let e = |d: &[u8]| expand(d, "H").map(|v| String::from_utf8(v).unwrap());
        assert_eq!(e(b"$Id$").as_deref(), Some("$Id: H $"));
        assert_eq!(e(b"$Id$Id: b $").as_deref(), Some("$Id: H $Id: b $"));
        assert_eq!(e(b"$Id: foo $").as_deref(), Some("$Id: H $"));
        assert_eq!(e(b"$Id: file.c,v 1.2 $"), None);
        assert_eq!(e(b"$Id: a  $"), None);
        assert_eq!(e(b"$Id:  $").as_deref(), Some("$Id: H $"));
        assert_eq!(e(b"$Id:\tq w $").as_deref(), Some("$Id: H $"));
        assert_eq!(e(b"$Id: ab$").as_deref(), Some("$Id: H $"));
        assert_eq!(e(b"$Id"), None);
    }
}
