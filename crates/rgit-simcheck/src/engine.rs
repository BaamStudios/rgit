// SPDX-License-Identifier: MIT
//! k-gram fingerprinting with winnowing (Schleimer, Wilkerson, Aiken 2003,
//! "Winnowing: Local Algorithms for Document Fingerprinting", the MOSS
//! algorithm) plus a gap-tolerant chaining step that turns fingerprint hits
//! into matched regions with a score.

use std::collections::{HashMap, HashSet};

use crate::norm::{Seq, distinct_trigrams};

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// k-gram length in tokens.
    pub k: usize,
    /// Winnowing window; every match of at least `k + w - 1` tokens is seen.
    pub w: usize,
    /// Maximum distance (in tokens) between two k-gram hits of one region.
    pub gap: usize,
    /// k-grams occurring more often than this in one file are ignored as noise.
    pub max_occ: usize,
    /// Minimum number of matched tokens for a finding.
    pub threshold: usize,
    /// Minimum number of distinct token trigrams in the matched region.
    pub min_distinct: usize,
    /// k-grams found in more upstream files than this do not add to a score.
    pub max_df: usize,
}

pub fn kgram_hashes(ids: &[u32], k: usize) -> Vec<u64> {
    if ids.len() < k {
        return Vec::new();
    }
    ids.windows(k)
        .map(|w| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for &id in w {
                h ^= u64::from(id).wrapping_add(0x9e37_79b9_7f4a_7c15);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
                h ^= h >> 29;
            }
            h
        })
        .collect()
}

/// Winnowing: from every window of `w` consecutive hashes keep the minimum
/// (rightmost on ties), recording each selected position once.
pub fn winnow(hashes: &[u64], w: usize) -> Vec<(u64, u32)> {
    let mut out: Vec<(u64, u32)> = Vec::new();
    if hashes.is_empty() {
        return out;
    }
    let w = w.max(1).min(hashes.len());
    for start in 0..=hashes.len() - w {
        let mut best = start;
        for p in start..start + w {
            if hashes[p] <= hashes[best] {
                best = p;
            }
        }
        if out.last().map(|l| l.1) != Some(best as u32) {
            out.push((hashes[best], best as u32));
        }
    }
    out
}

pub struct Doc {
    pub path: String,
    pub seq: Seq,
    /// Overrides `Params::threshold` for matches against this document.
    pub threshold: Option<usize>,
}

/// The reference corpus (upstream git) with its fingerprint index.
pub struct Corpus {
    pub docs: Vec<Doc>,
    hashes: Vec<Vec<u64>>,
    index: HashMap<u64, Vec<(u32, u32)>>,
    /// Number of upstream documents containing each k-gram.
    df: HashMap<u64, u32>,
    params: Params,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub up_doc: usize,
    /// Token range in the checked document (end exclusive).
    pub r: (usize, usize),
    /// Token range in the upstream document (end exclusive).
    pub u: (usize, usize),
    /// Matched tokens of the checked document.
    pub score: usize,
}

impl Hit {
    pub fn coverage(&self) -> f64 {
        self.score as f64 / (self.r.1 - self.r.0).max(1) as f64
    }
}

struct Chain {
    first: (usize, usize),
    last: (usize, usize),
    /// Rgit position of the last k-gram that added to `covered`.
    last_counted: Option<usize>,
    covered: usize,
    seen: HashSet<u64>,
}

impl Corpus {
    pub fn new(docs: Vec<Doc>, params: Params) -> Corpus {
        let mut hashes = Vec::with_capacity(docs.len());
        let mut index: HashMap<u64, Vec<(u32, u32)>> = HashMap::new();
        let mut df: HashMap<u64, u32> = HashMap::new();
        for (d, doc) in docs.iter().enumerate() {
            let h = kgram_hashes(&doc.seq.ids, params.k);
            for (hash, pos) in winnow(&h, params.w) {
                index.entry(hash).or_default().push((d as u32, pos));
            }
            let distinct: HashSet<u64> = h.iter().copied().collect();
            for hash in distinct {
                *df.entry(hash).or_default() += 1;
            }
            hashes.push(h);
        }
        Corpus {
            docs,
            hashes,
            index,
            df,
            params,
        }
    }

    pub fn params(&self) -> Params {
        self.params
    }

    /// Tokens of `seq` covered by distinct k-grams that occur upstream and are
    /// specific enough to count (see `Params::max_df`): an upper bound for
    /// the score `seq` can reach.
    pub fn specific_coverage(&self, seq: &Seq) -> usize {
        let p = self.params;
        let mut seen = HashSet::new();
        let mut last: Option<usize> = None;
        let mut covered = 0;
        for (i, h) in kgram_hashes(&seq.ids, p.k).into_iter().enumerate() {
            let df = self.df.get(&h).copied().unwrap_or(0) as usize;
            if (1..=p.max_df).contains(&df) && seen.insert(h) {
                covered += last.map_or(p.k, |l| (i - l).min(p.k));
                last = Some(i);
            }
        }
        covered
    }

    /// All regions of `seq` that match some upstream document.
    pub fn find(&self, seq: &Seq) -> Vec<Hit> {
        self.find_with(seq, None)
    }

    /// Like `find`, with an optional threshold overriding all others
    /// (used by the self-test to report near misses).
    pub fn find_with(&self, seq: &Seq, threshold: Option<usize>) -> Vec<Hit> {
        let p = self.params;
        let h = kgram_hashes(&seq.ids, p.k);
        // Candidate documents: every upstream fingerprint that is a k-gram of
        // `seq`. Winnowing guarantees that each shared run of at least
        // k + w - 1 tokens contains one of upstream's fingerprints.
        let mut cand: HashMap<u32, usize> = HashMap::new();
        let distinct: HashSet<u64> = h.iter().copied().collect();
        for hash in &distinct {
            if let Some(v) = self.index.get(hash) {
                for &(d, _) in v {
                    *cand.entry(d).or_default() += 1;
                }
            }
        }
        let mut local: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, &hash) in h.iter().enumerate() {
            local.entry(hash).or_default().push(i);
        }
        local.retain(|_, v| v.len() <= p.max_occ);

        let mut hits = Vec::new();
        let mut cand: Vec<_> = cand.into_iter().collect();
        cand.sort_unstable();
        for (d, _) in cand {
            let d = d as usize;
            let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
            for (j, hash) in self.hashes[d].iter().enumerate() {
                if local.contains_key(hash) {
                    by_hash.entry(*hash).or_default().push(j);
                }
            }
            let mut pairs = Vec::new();
            for (hash, js) in &by_hash {
                if js.len() > p.max_occ {
                    continue;
                }
                // k-grams found in many upstream files are generic idioms:
                // they may link a region but do not add to its score.
                let counts = self.df.get(hash).copied().unwrap_or(0) as usize <= p.max_df;
                for &i in &local[hash] {
                    for &j in js {
                        pairs.push((i, j, *hash, counts));
                    }
                }
            }
            pairs.sort_unstable_by_key(|x| (x.0, x.1));
            let min = threshold.or(self.docs[d].threshold).unwrap_or(p.threshold);
            for c in chain(&pairs, p) {
                if c.covered < min {
                    continue;
                }
                let r = (c.first.0, c.last.0 + p.k);
                if distinct_trigrams(&seq.ids[r.0..r.1]) < p.min_distinct {
                    continue;
                }
                hits.push(Hit {
                    up_doc: d,
                    r,
                    u: (c.first.1, c.last.1 + p.k),
                    score: c.covered,
                });
            }
        }
        dedupe(hits)
    }
}

/// Chain k-gram matches `(rgit pos, upstream pos, hash, counts)` into
/// regions, tolerating gaps of up to `p.gap` tokens on both sides. A region's
/// score is the number of rgit tokens covered by distinct, specific k-grams,
/// so repeating the same line many times does not inflate it.
fn chain(pairs: &[(usize, usize, u64, bool)], p: Params) -> Vec<Chain> {
    let mut active: Vec<Chain> = Vec::new();
    let mut done: Vec<Chain> = Vec::new();
    // `pairs` is sorted by rgit position; handle all pairs of one position
    // together so that each chain picks its best continuation.
    for group in pairs.chunk_by(|a, b| a.0 == b.0) {
        let i = group[0].0;
        let mut k = 0;
        while k < active.len() {
            if active[k].last.0 + p.gap < i {
                done.push(active.swap_remove(k));
            } else {
                k += 1;
            }
        }
        let mut used = vec![false; group.len()];
        for c in &mut active {
            let di = i - c.last.0;
            let best = group
                .iter()
                .enumerate()
                .filter(|(_, x)| (1..=p.gap).contains(&x.1.wrapping_sub(c.last.1)))
                .min_by_key(|(_, x)| x.1.wrapping_sub(c.last.1).abs_diff(di));
            if let Some((g, &(_, j, hash, counts))) = best {
                used[g] = true;
                c.extend(i, j, hash, counts, p.k);
            }
        }
        for (g, &(_, j, hash, counts)) in group.iter().enumerate() {
            if !used[g] {
                let mut c = Chain {
                    first: (i, j),
                    last: (i, j),
                    last_counted: None,
                    covered: 0,
                    seen: HashSet::new(),
                };
                c.extend(i, j, hash, counts, p.k);
                active.push(c);
            }
        }
    }
    done.extend(active);
    done
}

impl Chain {
    fn extend(&mut self, i: usize, j: usize, hash: u64, counts: bool, k: usize) {
        self.last = (i, j);
        if counts && self.seen.insert(hash) {
            self.covered += self.last_counted.map_or(k, |l| (i - l).min(k));
            self.last_counted = Some(i);
        }
    }
}

/// Keep the best hit for each region of the checked document.
fn dedupe(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.r.cmp(&b.r)));
    let mut kept: Vec<Hit> = Vec::new();
    for h in hits {
        let len = h.r.1 - h.r.0;
        let overlaps = kept.iter().any(|k| {
            let lo = h.r.0.max(k.r.0);
            let hi = h.r.1.min(k.r.1);
            hi > lo && (hi - lo) * 2 > len
        });
        if !overlaps {
            kept.push(h);
        }
    }
    kept.sort_by_key(|h| h.r);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winnow_guarantee() {
        let ids: Vec<u32> = (0..200).map(|x| (x * 7919 % 31) as u32).collect();
        let h = kgram_hashes(&ids, 5);
        let fp = winnow(&h, 4);
        // Every window of 4 hashes contains a selected position.
        for s in 0..=h.len() - 4 {
            assert!(fp.iter().any(|f| (s..s + 4).contains(&(f.1 as usize))));
        }
    }

    #[test]
    fn finds_copy_with_edits() {
        let params = Params {
            k: 6,
            w: 4,
            gap: 12,
            max_occ: 20,
            threshold: 30,
            min_distinct: 10,
            max_df: 10,
        };
        let mut rng = crate::selftest::Rng::new(7);
        let up: Vec<u32> = (0..300).map(|_| (rng.next() % 97) as u32).collect();
        let mut mine: Vec<u32> = (1000..1050).collect();
        mine.extend_from_slice(&up[100..130]);
        mine.push(5000); // one inserted token
        mine.extend_from_slice(&up[130..160]);
        mine.extend(2000..2050);
        let seq = |ids: Vec<u32>| Seq {
            lines: (0..ids.len() as u32).collect(),
            ids,
        };
        let corpus = Corpus::new(
            vec![Doc {
                path: "u".into(),
                seq: seq(up),
                threshold: None,
            }],
            params,
        );
        let hits = corpus.find(&seq(mine));
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].score >= 50);
        assert_eq!(hits[0].u.0, 100);
    }
}
