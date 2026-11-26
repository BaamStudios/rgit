// SPDX-License-Identifier: MIT

//! The deltas of `delta <size>` hunks in binary patches.
//!
//! The instruction format is the one of `Documentation/gitformat-pack.adoc`
//! ("Deltified representation"). Which instructions `git diff --binary`
//! writes was derived from differential experiments against the `git`
//! binary (see `tests/binary_delta.rs`):
//!
//! - The source is indexed at every 16th position `p` (16, 32, 48, ...),
//!   keyed by a hash of the 16 bytes that end with `p` (`src[p - 15..=p]`).
//!   A window hashing like the one 16 bytes earlier is left out, so a long
//!   run of equal windows keeps only its first entry. Hash buckets holding
//!   more than 64 entries are thinned out (see `index` and `POLY`).
//! - The target is scanned from position 16 on. At position `t` the 16
//!   bytes ending with `t` are looked up; each source entry with that hash
//!   is compared forwards from `p` and `t`. The longest match wins (the
//!   lowest source position on ties); shorter than 4 bytes counts as no
//!   match. A match of 4096 bytes or more ends the search early.
//! - A match grows backwards over the bytes still waiting to be inserted,
//!   then becomes copies of at most 64 KiB each; the scan goes on after
//!   them.
//! - Unmatched bytes become insert instructions of at most 127 bytes; an
//!   insert is closed as soon as it holds 127 bytes, so a later match
//!   cannot grow back into it.
//! - Copies name only the non-zero bytes of offset and size; 64 KiB is
//!   written as size 0.

use crate::lines::FastMap;

/// Bytes per index window.
const WINDOW: usize = 16;
/// Shortest forward match that becomes a copy.
const MIN_MATCH: usize = 4;
/// A forward match this long ends the search through the candidates.
const GOOD_MATCH: usize = 4096;
/// Longest copy instruction.
const MAX_COPY: usize = 0x10000;
/// Longest insert instruction.
const MAX_INSERT: usize = 127;
/// Entries kept per hash bucket.
const BUCKET_LIMIT: usize = 64;
/// The window hash: the window as a polynomial over GF(2) (first byte
/// highest, bit 0 of each byte its lowest coefficient) modulo this
/// polynomial of degree 31. It was recovered from the bucket sharing
/// observed through the thinning of full buckets (linear in the window
/// bits, bucket = low bits), with Berlekamp-Massey over the measured low
/// bits of the powers of x.
const POLY: u32 = 0xab59_b4d1;

/// `TABLES[i][b]`: the hash of a window whose only non-zero byte is `b` at
/// index `i`. The hash is linear, so a window hashes to the XOR of its
/// bytes' entries.
static TABLES: std::sync::LazyLock<Box<[[u32; 256]; WINDOW]>> = std::sync::LazyLock::new(|| {
    let mut t = Box::new([[0u32; 256]; WINDOW]);
    for (i, row) in t.iter_mut().enumerate() {
        for (b, slot) in row.iter_mut().enumerate() {
            let mut v = b as u32;
            for _ in 0..8 * (WINDOW - 1 - i) {
                v <<= 1;
                if v & (1 << 31) != 0 {
                    v ^= POLY;
                }
            }
            *slot = v;
        }
    }
    t
});

/// The hash of the window that ends with `data[end]`.
fn window_hash(data: &[u8], end: usize) -> u32 {
    let t = &*TABLES;
    data[end + 1 - WINDOW..=end]
        .iter()
        .zip(t.iter())
        .fold(0, |h, (&b, row)| h ^ row[usize::from(b)])
}

/// The source positions per window hash, lowest first.
///
/// Entries go into a table of buckets (a power of two, at least a quarter
/// of the number of window positions, rounded down; bucket = low bits of
/// the hash), in source order. A bucket with `c` entries, more than
/// [`BUCKET_LIMIT`], drops its entries number `1 + i * c / (c - 64)` for
/// `i` in `0..c - 64`.
fn index(src: &[u8]) -> FastMap<u32, Vec<u32>> {
    let positions = src.len().saturating_sub(1) / WINDOW;
    let size = (positions / 4).max(1).next_power_of_two();
    let mask = (size - 1) as u32;
    let mut buckets: Vec<Vec<(u32, u32)>> = vec![Vec::new(); size];
    let mut prev = None;
    for p in (WINDOW..src.len()).step_by(WINDOW) {
        let h = window_hash(src, p);
        if prev == Some(h) {
            continue;
        }
        prev = Some(h);
        buckets[(h & mask) as usize].push((h, p as u32));
    }
    let mut map: FastMap<u32, Vec<u32>> = FastMap::default();
    for bucket in &buckets {
        let c = bucket.len();
        let surplus = c.saturating_sub(BUCKET_LIMIT);
        // Index of the next entry to drop and how many were dropped.
        let mut dropped = 0;
        let mut next = if surplus > 0 { 1 } else { usize::MAX };
        for (k, &(h, p)) in bucket.iter().enumerate() {
            if k == next {
                dropped += 1;
                next = if dropped < surplus {
                    1 + dropped * c / surplus
                } else {
                    usize::MAX
                };
                continue;
            }
            map.entry(h).or_default().push(p);
        }
    }
    map
}

fn push_size(out: &mut Vec<u8>, mut v: usize) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn push_insert(out: &mut Vec<u8>, data: &[u8]) {
    if !data.is_empty() {
        out.push(data.len() as u8);
        out.extend_from_slice(data);
    }
}

fn push_copy(out: &mut Vec<u8>, offset: usize, size: usize) {
    let at = out.len();
    out.push(0x80);
    let mut op = 0x80u8;
    for k in 0..4 {
        let b = (offset >> (8 * k)) as u8;
        if b != 0 {
            op |= 1 << k;
            out.push(b);
        }
    }
    // A size of 64 KiB is written as no size bytes at all (size 0).
    let size = size & 0xffff;
    for k in 0..2 {
        let b = (size >> (8 * k)) as u8;
        if b != 0 {
            op |= 0x10 << k;
            out.push(b);
        }
    }
    out[at] = op;
}

/// The delta that turns `src` into `tgt`, as `git diff --binary` writes
/// it (before compression); `None` once it would grow past `max_size`
/// bytes. Bytes waiting to be inserted count as written, even when a
/// later copy takes them back.
pub fn make_delta(src: &[u8], tgt: &[u8], max_size: usize) -> Option<Vec<u8>> {
    let idx = index(src);
    let mut out = Vec::new();
    push_size(&mut out, src.len());
    push_size(&mut out, tgt.len());
    // Start of the bytes waiting to be inserted.
    let mut pending = 0;
    let mut t = 0;
    while t < tgt.len() {
        let mut best = 0;
        let mut best_p = 0;
        if t >= WINDOW
            && let Some(list) = idx.get(&window_hash(tgt, t))
        {
            let limit = tgt.len() - t;
            for &p in list {
                let p = p as usize;
                let n = src[p..]
                    .iter()
                    .zip(&tgt[t..t + limit])
                    .take_while(|(a, b)| a == b)
                    .count();
                if n > best {
                    best = n;
                    best_p = p;
                    if best >= GOOD_MATCH {
                        break;
                    }
                }
            }
        }
        if best >= MIN_MATCH {
            let (mut p, mut s) = (best_p, t);
            while s > pending && p > 0 && src[p - 1] == tgt[s - 1] {
                p -= 1;
                s -= 1;
                best += 1;
            }
            push_insert(&mut out, &tgt[pending..s]);
            while best > 0 {
                let size = best.min(MAX_COPY);
                push_copy(&mut out, p, size);
                p += size;
                s += size;
                best -= size;
            }
            t = s;

            pending = t;
            if out.len() > max_size {
                return None;
            }
        } else {
            t += 1;
            // The first window is inserted unchecked; afterwards the size is
            // checked after every byte and every copy.
            if t > WINDOW && out.len() + 1 + t - pending > max_size {
                return None;
            }
            if t - pending == MAX_INSERT {
                push_insert(&mut out, &tgt[pending..t]);
                pending = t;
            }
        }
    }
    push_insert(&mut out, &tgt[pending..]);
    (out.len() <= max_size).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies a delta (for the tests only).
    fn apply(src: &[u8], delta: &[u8]) -> Vec<u8> {
        fn size(d: &[u8], i: &mut usize) -> usize {
            let mut v = 0;
            let mut shift = 0;
            loop {
                let b = d[*i];
                *i += 1;
                v |= usize::from(b & 0x7f) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    return v;
                }
            }
        }
        let mut i = 0;
        assert_eq!(size(delta, &mut i), src.len());
        let n = size(delta, &mut i);
        let mut out = Vec::new();
        while i < delta.len() {
            let op = delta[i];
            i += 1;
            if op & 0x80 != 0 {
                let mut off = 0;
                let mut len = 0;
                for k in 0..4 {
                    if op & (1 << k) != 0 {
                        off |= usize::from(delta[i]) << (8 * k);
                        i += 1;
                    }
                }
                for k in 0..3 {
                    if op & (0x10 << k) != 0 {
                        len |= usize::from(delta[i]) << (8 * k);
                        i += 1;
                    }
                }
                if len == 0 {
                    len = 0x10000;
                }
                out.extend_from_slice(&src[off..off + len]);
            } else {
                out.extend_from_slice(&delta[i..i + usize::from(op)]);
                i += usize::from(op);
            }
        }
        assert_eq!(out.len(), n);
        out
    }

    fn pseudo_random(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn round_trips() {
        let a = pseudo_random(100_000, 1);
        let mut b = a[..30_000].to_vec();
        b.extend_from_slice(b"inserted");
        b.extend_from_slice(&a[31_000..]);
        b.extend(std::iter::repeat_n(0u8, 300));
        b.extend_from_slice(&a[..5000]);
        for (s, t) in [(&a, &b), (&b, &a)] {
            assert_eq!(apply(s, &make_delta(s, t, usize::MAX).unwrap()), *t);
        }
        let zeros = vec![0u8; 200_000];
        let mut z2 = zeros.clone();
        z2[70_000] = 1;
        assert_eq!(
            apply(&zeros, &make_delta(&zeros, &z2, usize::MAX).unwrap()),
            z2
        );
        assert_eq!(
            apply(b"", &make_delta(b"", b"abc", usize::MAX).unwrap()),
            b"abc"
        );
        assert_eq!(
            apply(b"abc", &make_delta(b"abc", b"", usize::MAX).unwrap()),
            b""
        );
    }

    #[test]
    fn instruction_choices() {
        // A copy found at the second window grows back to the start; an
        // insert closed at 127 bytes is not taken back.
        let src = pseudo_random(1000, 7);
        let mut tgt = pseudo_random(126, 9);
        tgt.extend_from_slice(&src[..600]);
        let d = make_delta(&src, &tgt, usize::MAX).unwrap();
        assert_eq!(make_delta(&src, &tgt, 100), None);
        // Sizes, insert 127 (126 + src[0]), copy of 599 bytes from 1.
        assert_eq!(d[4], 127);
        assert_eq!(&d[4 + 128..], &[0xb1, 1, 0x57, 0x02]);
    }
}
