// SPDX-License-Identifier: MIT
//! The similarity estimate shared by break and rename detection.
//!
//! gitdiffcore(7) describes it only as "the extent of changes": how much
//! of the old contents survives in the new contents. The details below
//! were determined by differential tests against the `git` binary:
//!
//! - a file is cut into pieces that end after a newline or after 64
//!   bytes, whichever comes first;
//! - in a text file (no NUL byte near the start) a carriage return
//!   directly before a newline is not part of any piece;
//! - each distinct piece is weighed by its length, and the amount of
//!   copied material is the sum over all distinct pieces of the smaller of
//!   the two weights (so repeated lines count as often as they occur on
//!   both sides);
//! - the similarity is the copied amount relative to the size of the
//!   larger of the two files, in units of [`super::MAX_SCORE`].

use crate::is_binary;

/// Longest piece.
const PIECE: usize = 64;

/// The pieces of one file: `(hash, weight)` sorted by hash, and the size
/// of the file.
#[derive(Debug, Clone, Default)]
pub struct Pieces {
    pub size: u64,
    counts: Vec<(u64, u64)>,
}

fn mix(h: u64, c: u8) -> u64 {
    // FNV-1a, 64 bit.
    (h ^ u64::from(c)).wrapping_mul(0x0000_0100_0000_01b3)
}

const SEED: u64 = 0xcbf2_9ce4_8422_2325;

impl Pieces {
    /// Cut `data` into pieces.
    pub fn new(data: &[u8]) -> Pieces {
        let text = !is_binary(data);
        let mut map: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        let mut h = SEED;
        let mut len = 0usize;
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            i += 1;
            if text && c == b'\r' && data.get(i) == Some(&b'\n') {
                continue;
            }
            h = mix(h, c);
            len += 1;
            if c == b'\n' || len == PIECE {
                *map.entry(h ^ ((len as u64) << 56)).or_insert(0) += len as u64;
                h = SEED;
                len = 0;
            }
        }
        if len > 0 {
            *map.entry(h ^ ((len as u64) << 56)).or_insert(0) += len as u64;
        }
        let mut counts: Vec<(u64, u64)> = map.into_iter().collect();
        counts.sort_unstable();
        Pieces {
            size: data.len() as u64,
            counts,
        }
    }

    /// The amount of material of `self` found in `other`.
    pub fn copied(&self, other: &Pieces) -> u64 {
        let (a, b) = (&self.counts, &other.counts);
        let (mut i, mut j) = (0, 0);
        let mut sum = 0;
        while i < a.len() && j < b.len() {
            match a[i].0.cmp(&b[j].0) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    sum += a[i].1.min(b[j].1);
                    i += 1;
                    j += 1;
                }
            }
        }
        sum
    }
}

/// The similarity of two files in units of [`super::MAX_SCORE`]; 0 when it
/// cannot reach `minimum` (checked from the sizes first).
pub fn similarity(src: &Pieces, dst: &Pieces, minimum: u32) -> u32 {
    let max = src.size.max(dst.size);
    let min = src.size.min(dst.size);
    if max == 0 {
        return super::MAX_SCORE;
    }
    let max_score = u64::from(super::MAX_SCORE);
    if min * max_score < u64::from(minimum) * max {
        return 0;
    }
    let copied = src.copied(dst);
    u32::try_from(copied * max_score / max).unwrap_or(super::MAX_SCORE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize, pre: &str) -> String {
        (0..n).map(|i| format!("{pre} {i}\n")).collect()
    }

    fn pct(a: &str, b: &str) -> u32 {
        similarity(&Pieces::new(a.as_bytes()), &Pieces::new(b.as_bytes()), 0) * 100
            / super::super::MAX_SCORE
    }

    #[test]
    fn changed_lines() {
        let src = lines(20, "line");
        let mut l: Vec<String> = src.lines().map(|l| format!("{l}\n")).collect();
        l[0] = "CHANGED 0\n".into();
        assert_eq!(pct(&src, &l.concat()), 93);
        l[1] = "CHANGED 1\n".into();
        assert_eq!(pct(&src, &l.concat()), 87);
    }

    #[test]
    fn long_lines_and_crlf() {
        let s = format!("{}\n", "x".repeat(200));
        let d = format!("{}y\n", "x".repeat(199));
        assert_eq!(pct(&s, &d), 95);
        let s = lines(20, "line");
        assert_eq!(pct(&s, &s.replace('\n', "\r\n")), 88);
        let d = s.replace("line 0\n", "LINE X\r\n");
        assert_eq!(pct(&s, &d), 94);
    }

    #[test]
    fn repeated_lines() {
        let s = "same\n".repeat(10);
        let d = format!("{}{}", "same\n".repeat(5), "other\n".repeat(5));
        assert_eq!(pct(&s, &d), 45);
        assert_eq!(pct(&s, &"same\n".repeat(20)), 50);
    }
}
