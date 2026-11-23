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
    /// 16 bytes per step, each mixed with one wide multiplication (the
    /// "folded multiply" of wyhash-style hashes); the last up to 15 bytes
    /// are read as two possibly overlapping words, or one padded word.
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        const K1: u64 = 0xa076_1d64_78bd_642f;
        const K2: u64 = 0xe703_7ed1_a0b4_28db;
        #[inline]
        fn fold(a: u64, b: u64) -> u64 {
            let r = u128::from(a) * u128::from(b);
            (r as u64) ^ ((r >> 64) as u64)
        }
        #[inline]
        fn word(b: &[u8]) -> u64 {
            u64::from_le_bytes(b[..8].try_into().expect("8 bytes"))
        }
        let mut h = self.0;
        let (chunks, rest) = bytes.as_chunks::<16>();
        for c in chunks {
            h = fold(word(&c[..8]) ^ h ^ K1, word(&c[8..]) ^ K2);
        }
        let n = rest.len();
        let (a, b) = if n >= 8 {
            (word(rest), word(&rest[n - 8..]))
        } else {
            // Fewer than 8 bytes: one zero-padded word.
            let mut padded = [0u8; 8];
            padded[..n].copy_from_slice(rest);
            (u64::from_le_bytes(padded), 0)
        };
        self.0 = fold(a ^ h ^ K1, b ^ K2 ^ (n as u64));
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
    for p in memchr::memchr_iter(b'\n', data) {
        lines.push(&data[start..=p]);
        start = p + 1;
    }
    if start < data.len() {
        lines.push(&data[start..]);
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
    // The common leading and trailing lines of `b` take the ids of their
    // copies in `a` without being hashed again (ids are handed out in
    // the same order as when every line is looked up).
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    SLOTS.with_borrow_mut(|slots| {
        let mut table = LineTable::new(slots, a.len() + b.len() - pre - suf);
        let ia: Vec<u32> = a.iter().map(|l| table.id(l)).collect();
        let mut ib: Vec<u32> = Vec::with_capacity(b.len());
        ib.extend_from_slice(&ia[..pre]);
        ib.extend(b[pre..b.len() - suf].iter().map(|l| table.id(l)));
        ib.extend_from_slice(&ia[a.len() - suf..]);
        let classes = table.lines.len();
        if slots.len() > MAX_KEPT_SLOTS {
            *slots = Vec::new();
        }
        (ia, ib, classes)
    })
}

/// Tables of up to this many slots are kept for the next diff.
const MAX_KEPT_SLOTS: usize = 1 << 20;

thread_local! {
    /// The slots of [`LineTable`], kept between diffs: `log -p` diffs
    /// many files, and a fresh table for each costs more than the lookups.
    static SLOTS: std::cell::RefCell<Vec<(u32, u32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Ids of distinct lines in first-seen order: an open-addressing table
/// (linear probing, at most half full) of (hash bits, id + 1) slots.
struct LineTable<'s, 'x> {
    slots: &'s mut Vec<(u32, u32)>,
    mask: usize,
    /// The first line of each id.
    lines: Vec<&'x [u8]>,
}

impl<'s, 'x> LineTable<'s, 'x> {
    fn new(slots: &'s mut Vec<(u32, u32)>, max_lines: usize) -> LineTable<'s, 'x> {
        let size = (max_lines * 2).next_power_of_two().max(16);
        slots.clear();
        slots.resize(size, (0, 0));
        LineTable {
            slots,
            mask: size - 1,
            lines: Vec::new(),
        }
    }

    #[inline]
    fn id(&mut self, line: &'x [u8]) -> u32 {
        let mut h = LineHasher::default();
        h.write(line);
        let h = h.finish();
        let tag = (h >> 32) as u32;
        let mut i = h as usize & self.mask;
        loop {
            let (t, v) = self.slots[i];
            if v == 0 {
                self.lines.push(line);
                let id = self.lines.len() as u32;
                self.slots[i] = (tag, id);
                return id - 1;
            }
            if t == tag && self.lines[v as usize - 1] == line {
                return v - 1;
            }
            i = (i + 1) & self.mask;
        }
    }
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
