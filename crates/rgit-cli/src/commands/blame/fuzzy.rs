// SPDX-License-Identifier: MIT
//! Matching the changed lines of an ignored commit (`--ignore-rev`) to
//! lines of its parent.
//!
//! The lines of one changed region of the commit's version are matched
//! with the lines of the region they replace in the parent. A line is
//! described by the multiset of the byte pairs it contains (lower case,
//! whitespace as a separator that also surrounds the line; pairs of two
//! separators do not count); the similarity of two lines is the size of
//! the intersection of their multisets. Each line only looks at parent
//! lines up to [`MAX_DISTANCE`] away from the line at the same relative
//! position in the region. Its best candidate is the most similar one
//! (the nearer one when equally similar, then the earlier one). The line
//! whose best candidate stands out most from its second best is matched
//! first; the pairs of the matched line are taken out of the parent line
//! (so that a parent line split into several lines can match all of them)
//! and the lines before and after are matched in the same way, within the
//! parent lines before and after (including) the matched one. Lines that
//! have nothing in common with any candidate stay unmatched.
//!
//! The scoring details were derived from the behaviour of the `git`
//! binary (t8014-blame-ignore-fuzzy.sh and randomized comparisons).

use std::collections::HashMap;
use std::ops::Range;

/// How far from the line at the same relative position a candidate may be.
const MAX_DISTANCE: usize = 10;

/// Weight of the similarity against the distance in a candidate's score.
const WEIGHT: i64 = 1 << 20;

/// The byte pairs of a line, with their counts.
#[derive(Clone, Default)]
struct Fingerprint(HashMap<u16, u32>);

impl Fingerprint {
    fn of(line: &[u8]) -> Fingerprint {
        let mut map = HashMap::new();
        let norm = |b: u8| -> u8 {
            if b.is_ascii_whitespace() || b == 0x0b {
                0
            } else {
                b.to_ascii_lowercase()
            }
        };
        let mut prev = 0u8;
        for b in line.iter().copied().map(norm).chain(std::iter::once(0)) {
            if prev != 0 || b != 0 {
                *map.entry(u16::from(prev) << 8 | u16::from(b)).or_insert(0) += 1;
            }
            prev = b;
        }
        Fingerprint(map)
    }

    fn similarity(&self, other: &Fingerprint) -> u32 {
        let (small, big) = if self.0.len() <= other.0.len() {
            (self, other)
        } else {
            (other, self)
        };
        small
            .0
            .iter()
            .map(|(k, &n)| big.0.get(k).map_or(0, |&m| n.min(m)))
            .sum()
    }

    /// Take the pairs of `other` out of this one.
    fn subtract(&mut self, other: &Fingerprint) {
        for (k, &n) in &other.0 {
            if let Some(m) = self.0.get_mut(k) {
                *m = m.saturating_sub(n);
                if *m == 0 {
                    self.0.remove(k);
                }
            }
        }
    }
}

/// Similarity a line needs with a parent line outside its changed region.
const MIN_ELSEWHERE: u32 = 10;

/// The lines of the parent version. Matching takes the pairs of matched
/// lines out of them, for all regions of one commit.
pub struct Parent {
    lines: Vec<Fingerprint>,
}

impl Parent {
    pub fn new(parent: &[&[u8]]) -> Parent {
        Parent {
            lines: parent.iter().map(|l| Fingerprint::of(l)).collect(),
        }
    }

    /// For each line of `target`, the line of the region `region` of the
    /// parent it is matched with (relative to the region).
    pub fn match_region(&mut self, region: Range<usize>, target: &[&[u8]]) -> Vec<Option<usize>> {
        let mut m = Matcher {
            parent: &mut self.lines[region],
            target: target.iter().map(|l| Fingerprint::of(l)).collect(),
            result: vec![None; target.len()],
        };
        let (n, p) = (m.target.len(), m.parent.len());
        m.run(0, n, 0, p);
        m.result
    }

    /// For a line that matched nothing in its region: the parent line
    /// with at least [`MIN_ELSEWHERE`] pairs in common, the most similar
    /// one, the nearest to `at` when equally similar, the later one when
    /// equally near.
    pub fn elsewhere(&mut self, line: &[u8], at: usize) -> Option<usize> {
        let fp = Fingerprint::of(line);
        let mut best: Option<(u32, usize, usize)> = None;
        for (j, p) in self.lines.iter().enumerate() {
            let sim = fp.similarity(p);
            if sim < MIN_ELSEWHERE {
                continue;
            }
            let d = j.abs_diff(at);
            if best.is_none_or(|(s, bd, _)| sim > s || (sim == s && d <= bd)) {
                best = Some((sim, d, j));
            }
        }
        best.map(|b| b.2)
    }
}

struct Matcher<'p> {
    parent: &'p mut [Fingerprint],
    target: Vec<Fingerprint>,
    result: Vec<Option<usize>>,
}

impl Matcher<'_> {
    /// The parent line at the same relative position as target line `i`.
    fn center(&self, i: usize) -> usize {
        let (n, m) = (self.target.len(), self.parent.len());
        (2 * i + 1) * m / (2 * n)
    }

    /// Match target lines `t0..t1` within parent lines `p0..p1`.
    fn run(&mut self, t0: usize, t1: usize, p0: usize, p1: usize) {
        let mut todo = vec![(t0, t1, p0, p1)];
        while let Some((t0, t1, p0, p1)) = todo.pop() {
            if t0 >= t1 || p0 >= p1 {
                continue;
            }
            // (certainty, target line, parent line)
            let mut pick: Option<(i64, usize, usize)> = None;
            for i in t0..t1 {
                let c = self.center(i);
                let lo = p0.max(c.saturating_sub(MAX_DISTANCE));
                let hi = p1.min(c + MAX_DISTANCE + 1);
                let mut best: Option<(i64, usize)> = None;
                let mut second = 0i64;
                for j in lo..hi {
                    let sim = self.target[i].similarity(&self.parent[j]);
                    let score = if sim == 0 {
                        0
                    } else {
                        i64::from(sim) * WEIGHT - j.abs_diff(c) as i64
                    };
                    match best {
                        Some((b, _)) if score <= b => second = second.max(score),
                        _ => {
                            if let Some((b, _)) = best {
                                second = second.max(b);
                            }
                            best = Some((score, j));
                        }
                    }
                }
                let Some((b, j)) = best.filter(|b| b.0 > 0) else {
                    continue;
                };
                let certainty = 2 * b - second;
                if pick.is_none_or(|p| certainty > p.0) {
                    pick = Some((certainty, i, j));
                }
            }
            let Some((_, i, j)) = pick else {
                continue;
            };
            self.result[i] = Some(j);
            let t = std::mem::take(&mut self.target[i]);
            self.parent[j].subtract(&t);
            self.target[i] = t;
            // Depth first, the lines before before the lines after: both
            // may take pairs out of parent line `j`.
            todo.push((i + 1, t1, j, p1));
            todo.push((t0, i, p0, j + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(a: &[&str], b: &[&str]) -> Vec<Option<usize>> {
        let a: Vec<&[u8]> = a.iter().map(|s| s.as_bytes()).collect();
        let b: Vec<&[u8]> = b.iter().map(|s| s.as_bytes()).collect();
        Parent::new(&a).match_region(0..a.len(), &b)
    }

    #[test]
    fn fingerprints() {
        let f = Fingerprint::of(b"Ab  c");
        // \0a ab b\0 \0c c\0
        assert_eq!(f.0.values().sum::<u32>(), 5);
        assert_eq!(f.similarity(&Fingerprint::of(b"ab")), 3);
    }

    #[test]
    fn order_and_ties() {
        assert_eq!(run(&["abc", "abc"], &["abcd", "abcd"]), vec![Some(0), Some(1)]);
        assert_eq!(run(&["abcdef", "ghijkl", "ab"], &["ghijk", "abcd"]), vec![Some(1), Some(2)]);
        assert_eq!(
            run(&["abcdefg", "ijklmn", "abcdefgh"], &["abcdefghx", "ijklm"]),
            vec![Some(0), Some(1)]
        );
        assert_eq!(run(&["abcdef", "abcdef", "ghijkl"], &["gh", "abcdefx"]), vec![None, Some(1)]);
        assert_eq!(run(&["ab"], &["ba"]), vec![None]);
    }
}
