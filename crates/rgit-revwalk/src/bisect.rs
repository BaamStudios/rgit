// SPDX-License-Identifier: MIT
//! Choosing the commit to test next in a bisection
//! (Documentation/git-bisect-lk2009.adoc, "Bisection algorithm" and "Skip
//! algorithm"; Documentation/rev-list-options.adoc, "Bisection Helpers").
//!
//! The candidates are the commits `rev-list <bad> --not <good>...` lists,
//! in that order. Each candidate gets a weight: the number of candidates it
//! reaches (itself included). The best commit to test splits the candidates
//! in two halves of similar size: it maximises `min(weight, all - weight)`,
//! its "distance".
//!
//! The list is processed oldest first (the reverse of the walk order). The
//! weights are found in two steps. Roots of the candidate graph weigh 1,
//! merges are counted by walking their ancestry. Then the rest is filled in
//! pass by pass: a commit whose only candidate parent has a weight gets
//! that weight plus one. As soon as a merge or a commit filled in this way
//! lies halfway (`|2 * weight - all| <= 1`), it is the answer; otherwise
//! the commit with the largest distance wins, the oldest on a tie.
//!
//! With `find_all` every distance is computed and the candidates come back
//! sorted by distance (largest first), then by object id.

use std::collections::HashMap;

use rgit_hash::ObjectId;

/// A candidate with its parents among the candidates.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: ObjectId,
    /// Indices into the candidate list.
    pub parents: Vec<usize>,
}

/// Build the candidate list from the walk's output (`list`, in walk order)
/// and the parents of each commit (`--first-parent` keeps only the first).
pub fn candidates(
    list: &[ObjectId],
    mut parents: impl FnMut(&ObjectId) -> Vec<ObjectId>,
    first_parent: bool,
) -> Vec<Candidate> {
    let index: HashMap<ObjectId, usize> = list.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    list.iter()
        .map(|id| {
            let mut ps = parents(id);
            if first_parent {
                ps.truncate(1);
            }
            let mut idx: Vec<usize> = Vec::new();
            for p in &ps {
                if let Some(&i) = index.get(p)
                    && !idx.contains(&i)
                {
                    idx.push(i);
                }
            }
            Candidate {
                id: *id,
                parents: idx,
            }
        })
        .collect()
}

/// The result of [`find`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bisection {
    /// Index of the commit to test (`None` when there are no candidates).
    pub best: Option<usize>,
    /// The weight of every candidate (0 where it was not computed).
    pub weights: Vec<usize>,
}

/// `min(weight, all - weight)`.
pub fn distance(weight: usize, all: usize) -> usize {
    weight.min(all.saturating_sub(weight))
}

fn halfway(weight: usize, all: usize) -> bool {
    (2 * weight).abs_diff(all) <= 1
}

/// The number of candidates `start` reaches, itself included.
fn count_reachable(cands: &[Candidate], start: usize, mark: &mut [u32], stamp: u32) -> usize {
    let mut stack = vec![start];
    mark[start] = stamp;
    let mut n = 0;
    while let Some(i) = stack.pop() {
        n += 1;
        for &p in &cands[i].parents {
            if mark[p] != stamp {
                mark[p] = stamp;
                stack.push(p);
            }
        }
    }
    n
}

/// Compute the weights and choose the commit to test. Without `find_all`
/// the search stops at the first commit halfway.
pub fn find(cands: &[Candidate], find_all: bool) -> Bisection {
    let all = cands.len();
    let mut weights: Vec<Option<usize>> = vec![None; all];
    let mut mark = vec![0u32; all];
    let mut stamp = 0;
    // Oldest first: the reverse of the walk order.
    for (i, c) in cands.iter().enumerate().rev() {
        match c.parents.len() {
            0 => weights[i] = Some(1),
            1 => {}
            _ => {
                stamp += 1;
                let w = count_reachable(cands, i, &mut mark, stamp);
                weights[i] = Some(w);
                if !find_all && halfway(w, all) {
                    return Bisection {
                        best: Some(i),
                        weights: weights.iter().map(|w| w.unwrap_or(0)).collect(),
                    };
                }
            }
        }
    }
    loop {
        let mut changed = false;
        for (i, c) in cands.iter().enumerate().rev() {
            if weights[i].is_some() {
                continue;
            }
            let Some(w) = weights[c.parents[0]] else {
                continue;
            };
            weights[i] = Some(w + 1);
            changed = true;
            if !find_all && halfway(w + 1, all) {
                return Bisection {
                    best: Some(i),
                    weights: weights.iter().map(|w| w.unwrap_or(0)).collect(),
                };
            }
        }
        if !changed {
            break;
        }
    }
    let weights: Vec<usize> = weights.iter().map(|w| w.unwrap_or(0)).collect();
    let mut best: Option<usize> = None;
    for i in (0..all).rev() {
        let d = distance(weights[i], all);
        if best.is_none_or(|b| d > distance(weights[b], all)) {
            best = Some(i);
        }
    }
    Bisection { best, weights }
}

/// The candidates sorted by distance (largest first), then by object id
/// (`--bisect-all`).
pub fn sorted(cands: &[Candidate], weights: &[usize]) -> Vec<usize> {
    let all = cands.len();
    let mut order: Vec<usize> = (0..all).collect();
    order.sort_by(|&a, &b| {
        distance(weights[b], all)
            .cmp(&distance(weights[a], all))
            .then_with(|| cands[a].id.cmp(&cands[b].id))
    });
    order
}

/// The estimated number of steps left for `all` candidates
/// (`bisect_steps`): `n - 1` or `n`, where `2^n <= all < 2^(n+1)`, the
/// larger one when `all` exceeds `2^n` by more than a third of `2^n`.
pub fn estimate_steps(all: usize) -> usize {
    if all < 3 {
        return 0;
    }
    let n = usize::BITS as usize - 1 - all.leading_zeros() as usize;
    let e = 1usize << n;
    let x = all - e;
    if e < 3 * x { n } else { n - 1 }
}

/// The values `--bisect-vars` prints for the chosen commit of weight
/// `weight` among `all` candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vars {
    pub nr: i64,
    pub good: i64,
    pub bad: i64,
    pub all: usize,
    pub steps: usize,
}

impl Vars {
    pub fn new(weight: usize, all: usize) -> Vars {
        let good = all as i64 - weight as i64 - 1;
        let bad = weight as i64 - 1;
        Vars {
            nr: good.max(bad),
            good,
            bad,
            all,
            steps: estimate_steps(all),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn id(n: u8) -> ObjectId {
        let mut b = [0u8; 20];
        b[19] = n;
        ObjectId::from_bytes(HashKind::Sha1, &b).unwrap()
    }

    /// A linear history of `n` commits, newest first.
    fn linear(n: u8) -> Vec<Candidate> {
        let list: Vec<ObjectId> = (1..=n).rev().map(id).collect();
        candidates(
            &list,
            |c| {
                let k = c.as_bytes()[19];
                if k > 1 { vec![id(k - 1)] } else { vec![] }
            },
            false,
        )
    }

    #[test]
    fn steps() {
        let expect = [
            (1, 0),
            (2, 0),
            (3, 1),
            (5, 1),
            (6, 2),
            (10, 2),
            (11, 3),
            (21, 3),
            (22, 4),
            (42, 4),
            (43, 5),
            (85, 5),
            (86, 6),
            (170, 6),
            (171, 7),
        ];
        for (all, steps) in expect {
            assert_eq!(estimate_steps(all), steps, "{all}");
        }
    }

    #[test]
    fn linear_midpoints() {
        // (length, chosen commit number, oldest = 1)
        for (n, want) in [
            (1, 1),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 2),
            (6, 3),
            (9, 4),
            (16, 8),
        ] {
            let c = linear(n);
            let b = find(&c, false);
            assert_eq!(c[b.best.unwrap()].id, id(want), "{n}");
        }
    }

    #[test]
    fn vars() {
        assert_eq!(
            Vars::new(4, 9),
            Vars {
                nr: 4,
                good: 4,
                bad: 3,
                all: 9,
                steps: 2
            }
        );
        assert_eq!(Vars::new(1, 1).good, -1);
    }
}
