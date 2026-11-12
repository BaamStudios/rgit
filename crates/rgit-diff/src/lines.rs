// SPDX-License-Identifier: MIT

//! Splitting file contents into lines and interning them.

use std::collections::HashMap;

/// Number of leading bytes inspected by [`is_binary`].
pub const BINARY_CHECK_BYTES: usize = 8000;

/// Returns `true` if `data` looks binary: it contains a NUL byte within the
/// first [`BINARY_CHECK_BYTES`] bytes.
pub fn is_binary(data: &[u8]) -> bool {
    let n = data.len().min(BINARY_CHECK_BYTES);
    data[..n].contains(&0)
}

/// Splits `data` into lines. Every line keeps its terminating `\n`; only the
/// last line may lack it (an "incomplete line").
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < data.len() {
        match data[start..].iter().position(|&c| c == b'\n') {
            Some(p) => {
                lines.push(&data[start..start + p + 1]);
                start += p + 1;
            }
            None => {
                lines.push(&data[start..]);
                start = data.len();
            }
        }
    }
    lines
}

/// Maps the lines of both sides to dense ids so that equal lines (bytes
/// including the line terminator) share an id.
pub(crate) fn intern<'x>(a: &[&'x [u8]], b: &[&'x [u8]]) -> (Vec<u32>, Vec<u32>, usize) {
    let mut map: HashMap<&'x [u8], u32> = HashMap::with_capacity(a.len() + b.len());
    let mut id = |line: &'x [u8]| -> u32 {
        let next = map.len() as u32;
        *map.entry(line).or_insert(next)
    };
    let ia: Vec<u32> = a.iter().map(|l| id(l)).collect();
    let ib: Vec<u32> = b.iter().map(|l| id(l)).collect();
    let classes = map.len();
    (ia, ib, classes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        assert!(split_lines(b"").is_empty());
        assert_eq!(split_lines(b"a\nb"), vec![&b"a\n"[..], &b"b"[..]]);
        assert_eq!(split_lines(b"a\n\n"), vec![&b"a\n"[..], &b"\n"[..]]);
    }

    #[test]
    fn binary() {
        assert!(is_binary(b"a\0b"));
        assert!(!is_binary(b"abc"));
        let mut v = vec![b'a'; BINARY_CHECK_BYTES];
        v.push(0);
        assert!(!is_binary(&v));
    }
}
