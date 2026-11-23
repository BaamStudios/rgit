// SPDX-License-Identifier: MIT

//! Splitting file contents into lines and interning them.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A fast, non-cryptographic hasher for interning lines: multiply-rotate
/// over 8-byte words. Lines come from the user's own files, so resistance
/// against crafted collisions is not needed.
#[derive(Default, Clone, Copy)]
pub(crate) struct LineHasher(u64);

const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

impl LineHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(SEED);
    }
}

impl Hasher for LineHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut w = [0u8; 8];
            w[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(w) ^ ((rest.len() as u64) << 59));
        }
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// A `HashMap` with [`LineHasher`].
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<LineHasher>>;

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

/// Gives every distinct line an id, across any number of texts (for
/// [`crate::diff_interned`]).
#[derive(Default)]
pub struct Interner {
    map: FastMap<Vec<u8>, u32>,
}

impl Interner {
    pub fn new() -> Interner {
        Interner::default()
    }

    /// The id of `line`.
    pub fn id(&mut self, line: &[u8]) -> u32 {
        if let Some(&id) = self.map.get(line) {
            return id;
        }
        let id = self.map.len() as u32;
        self.map.insert(line.to_vec(), id);
        id
    }

    /// One more than the largest id handed out.
    pub fn classes(&self) -> usize {
        self.map.len()
    }
}

/// Maps the lines of both sides to dense ids so that equal lines (bytes
/// including the line terminator) share an id. With whitespace options
/// lines are equal when their [`crate::whitespace::key`]s are.
pub(crate) fn intern<'x>(
    a: &[&'x [u8]],
    b: &[&'x [u8]],
    ws: &crate::Whitespace,
) -> (Vec<u32>, Vec<u32>, usize) {
    if ws.any() {
        let mut map: FastMap<Vec<u8>, u32> =
            FastMap::with_capacity_and_hasher(a.len() + b.len(), Default::default());
        let mut id = |line: &[u8]| -> u32 {
            let next = map.len() as u32;
            let k = crate::whitespace::key(line, ws).unwrap_or_default();
            *map.entry(k).or_insert(next)
        };
        let ia: Vec<u32> = a.iter().map(|l| id(l)).collect();
        let ib: Vec<u32> = b.iter().map(|l| id(l)).collect();
        let classes = map.len();
        return (ia, ib, classes);
    }
    let mut map: FastMap<&'x [u8], u32> =
        FastMap::with_capacity_and_hasher(a.len() + b.len(), Default::default());
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
