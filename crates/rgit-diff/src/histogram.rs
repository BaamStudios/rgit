// SPDX-License-Identifier: MIT

//! Histogram diff, after the published description of JGit's
//! `HistogramDiff` (an extension of patience diff that also uses common
//! lines which occur more than once, preferring those that occur least
//! often in the old side).
//!
//! For a region, the old side is indexed: for every line its positions and
//! the number of times it occurs. The new side is scanned from the top; for
//! every line that also occurs in the old side, each of its old positions is
//! grown into the longest run of equal lines on both sides. The run that is
//! longest, or that contains a line with fewer occurrences than the best run
//! so far, wins. The regions before and after the winning run are diffed the
//! same way. If every common line occurs too often, the region falls back
//! to Myers.
//!
//! Known gap (issue #53): git's choice among equally good runs differs in
//! some cases, mostly when both files end with the same lines (minimal
//! example: `p x q` against `q p q`, where git matches `p` and the last
//! `q`). On generated inputs about 1 % of small and 25 % of large file
//! pairs differ; on the last 500 commits of git.git 3 of 500 patches.

use std::collections::HashMap;

use crate::myers;

/// Occurrence count in the old side above which a region is not indexed
/// but diffed with Myers.
const MAX_CHAIN: usize = 64;

pub(crate) fn diff(a: &[u32], b: &[u32], classes: usize, minimal: bool) -> (Vec<bool>, Vec<bool>) {
    let mut ca = vec![false; a.len()];
    let mut cb = vec![false; b.len()];
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
        match best_run(a, a0, a1, b, b0, b1) {
            Lcs::Fallback => {
                let (fa, fb) = myers::diff(&a[a0..a1], &b[b0..b1], classes, minimal);
                ca[a0..a1].copy_from_slice(&fa);
                cb[b0..b1].copy_from_slice(&fb);
            }
            Lcs::None => {
                ca[a0..a1].iter_mut().for_each(|f| *f = true);
                cb[b0..b1].iter_mut().for_each(|f| *f = true);
            }
            Lcs::Run {
                a: (rs, re),
                b: (ss, se),
            } => {
                stack.push((re, a1, se, b1));
                stack.push((a0, rs, b0, ss));
            }
        }
    }
    (ca, cb)
}

enum Lcs {
    Fallback,
    None,
    Run {
        a: (usize, usize),
        b: (usize, usize),
    },
}

struct Occ {
    count: usize,
    /// Positions in the old side, ascending.
    pos: Vec<usize>,
}

fn best_run(a: &[u32], a0: usize, a1: usize, b: &[u32], b0: usize, b1: usize) -> Lcs {
    let mut index: HashMap<u32, Occ> = HashMap::new();
    for (i, &id) in a.iter().enumerate().take(a1).skip(a0) {
        let e = index.entry(id).or_insert(Occ {
            count: 0,
            pos: Vec::new(),
        });
        e.count += 1;
        e.pos.push(i);
    }
    let count = |i: usize| index[&a[i]].count;
    let mut best: Option<(usize, usize, usize, usize)> = None;
    let mut best_count = MAX_CHAIN + 1;
    let mut has_common = false;
    let mut j = b0;
    while j < b1 {
        let mut j_next = j + 1;
        if let Some(occ) = index.get(&b[j]) {
            has_common = true;
            if occ.count <= best_count {
                let mut p = 0;
                while p < occ.pos.len() {
                    let start = occ.pos[p];
                    let (mut rs, mut ss) = (start, j);
                    let (mut re, mut se) = (start + 1, j + 1);
                    let mut rc = occ.count;
                    while rs > a0 && ss > b0 && a[rs - 1] == b[ss - 1] {
                        rs -= 1;
                        ss -= 1;
                        if rc > 1 {
                            rc = rc.min(count(rs));
                        }
                    }
                    while re < a1 && se < b1 && a[re] == b[se] {
                        if rc > 1 {
                            rc = rc.min(count(re));
                        }
                        re += 1;
                        se += 1;
                    }
                    if j_next < se {
                        j_next = se;
                    }
                    let len = best.map_or(0, |r| r.1 - r.0);
                    if len < re - rs || rc < best_count {
                        best = Some((rs, re, ss, se));
                        best_count = rc;
                    }
                    // Later positions inside this run were covered by it.
                    p += 1;
                    while p < occ.pos.len() && occ.pos[p] < re {
                        p += 1;
                    }
                }
            }
        }
        j = j_next;
    }
    if has_common && best_count > MAX_CHAIN {
        return Lcs::Fallback;
    }
    match best {
        Some((rs, re, ss, se)) => Lcs::Run {
            a: (rs, re),
            b: (ss, se),
        },
        None => Lcs::None,
    }
}
