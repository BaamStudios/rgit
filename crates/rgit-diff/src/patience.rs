// SPDX-License-Identifier: MIT

//! Patience diff and its anchored variant.
//!
//! Patience diff (Bram Cohen's description): lines that occur exactly once
//! on both sides are matched with each other, the longest subsequence of
//! these matches that is in order on both sides is kept (found with
//! patience sorting), and the stretches between the kept matches are
//! diffed the same way. A stretch without such lines is diffed with Myers.
//!
//! Anchored diff (`--anchored=<text>`): a unique line that starts with one
//! of the anchor texts should not end up deleted or added. While the
//! matches are sorted into piles, an anchored match discards the matches
//! that conflict with it; of two conflicting anchored matches the first (in
//! the order of the old side) wins.
//!
//! Tie breaks and the handling of stretches were derived from differential
//! tests against the `git` binary.

use std::collections::HashMap;

use crate::myers;

/// Anchor texts with the lines of the old side they are tested against.
pub(crate) type Anchors<'x> = (&'x [&'x [u8]], &'x [Vec<u8>]);

pub(crate) fn diff(
    a: &[u32],
    b: &[u32],
    classes: usize,
    minimal: bool,
    anchors: Option<Anchors<'_>>,
) -> (Vec<bool>, Vec<bool>) {
    let mut ca = vec![false; a.len()];
    let mut cb = vec![false; b.len()];
    let is_anchor = |i: usize| -> bool {
        anchors.is_some_and(|(lines, texts)| texts.iter().any(|t| lines[i].starts_with(t)))
    };
    let mut stack = vec![(0usize, a.len(), 0usize, b.len())];
    while let Some((a0, a1, b0, b1)) = stack.pop() {
        if a0 == a1 {
            cb[b0..b1].iter_mut().for_each(|f| *f = true);
            continue;
        }
        if b0 == b1 {
            ca[a0..a1].iter_mut().for_each(|f| *f = true);
            continue;
        }
        let matches = unique_matches(a, a0, a1, b, b0, b1, &is_anchor);
        if matches.is_empty() {
            let (fa, fb) = myers::diff(&a[a0..a1], &b[b0..b1], classes, minimal);
            ca[a0..a1].copy_from_slice(&fa);
            cb[b0..b1].copy_from_slice(&fb);
            continue;
        }
        let kept = longest_increasing(&matches);
        // The stretches before, between and after the kept matches. Each
        // loses its common first lines; each stretch that ends at a match
        // also loses its common last lines (the stretch after the last
        // match keeps them). Pushed so that the first stretch comes first.
        let mut stretches = Vec::with_capacity(kept.len() + 1);
        let mut start = (a0, b0);
        for &(i, j, _) in &kept {
            stretches.push(trim(a, b, start, (i, j), true));
            start = (i + 1, j + 1);
        }
        stretches.push(trim(a, b, start, (a1, b1), false));
        stack.extend(stretches.into_iter().rev());
    }
    (ca, cb)
}

/// The stretch from `s` to `e` without its common first lines and, if
/// `back`, without its common last lines.
fn trim(
    a: &[u32],
    b: &[u32],
    (mut s0, mut s1): (usize, usize),
    (mut e0, mut e1): (usize, usize),
    back: bool,
) -> (usize, usize, usize, usize) {
    while s0 < e0 && s1 < e1 && a[s0] == b[s1] {
        s0 += 1;
        s1 += 1;
    }
    while back && s0 < e0 && s1 < e1 && a[e0 - 1] == b[e1 - 1] {
        e0 -= 1;
        e1 -= 1;
    }
    (s0, e0, s1, e1)
}

/// The lines of `a[a0..a1]` that occur exactly once there and exactly once
/// in `b[b0..b1]`, as `(i, j, anchored)` in the order of `a`.
fn unique_matches(
    a: &[u32],
    a0: usize,
    a1: usize,
    b: &[u32],
    b0: usize,
    b1: usize,
    is_anchor: &dyn Fn(usize) -> bool,
) -> Vec<(usize, usize, bool)> {
    // id -> (count in a, position in a, count in b, position in b)
    let mut seen: HashMap<u32, (u32, usize, u32, usize)> = HashMap::new();
    for (i, &id) in a.iter().enumerate().take(a1).skip(a0) {
        let e = seen.entry(id).or_insert((0, i, 0, 0));
        e.0 += 1;
    }
    for (j, &id) in b.iter().enumerate().take(b1).skip(b0) {
        if let Some(e) = seen.get_mut(&id) {
            e.2 += 1;
            e.3 = j;
        }
    }
    let mut out: Vec<(usize, usize, bool)> = seen
        .values()
        .filter(|e| e.0 == 1 && e.2 == 1)
        .map(|e| (e.1, e.3, is_anchor(e.1)))
        .collect();
    out.sort_unstable();
    out
}

/// Longest subsequence of `m` (sorted by the first component) that is also
/// increasing in the second, found with patience sorting.
///
/// An anchored match always goes in: the piles above the one it lands on
/// are dropped, and later matches that would land on or below its pile are
/// skipped.
fn longest_increasing(m: &[(usize, usize, bool)]) -> Vec<(usize, usize, bool)> {
    // tops[k]: index into m of the top card of pile k; prev: back links.
    let mut tops: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; m.len()];
    let mut floor: Option<usize> = None;
    for (n, &(_, j, anchored)) in m.iter().enumerate() {
        let k = tops.partition_point(|&t| m[t].1 < j);
        if floor.is_some_and(|f| k <= f) {
            continue;
        }
        if anchored {
            floor = Some(k);
            tops.truncate(k + 1);
        }
        if k > 0 {
            prev[n] = Some(tops[k - 1]);
        }
        if k == tops.len() {
            tops.push(n);
        } else {
            tops[k] = n;
        }
    }
    let mut out = Vec::with_capacity(tops.len());
    let mut cur = tops.last().copied();
    while let Some(n) = cur {
        out.push(m[n]);
        cur = prev[n];
    }
    out.reverse();
    out
}
