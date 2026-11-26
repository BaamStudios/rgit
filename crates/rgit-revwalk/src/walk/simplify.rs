// SPDX-License-Identifier: MIT
//! History simplification: TREESAME, the default mode, `--full-history`,
//! `--sparse`, `--show-pulls`, `--remove-empty`, parent rewriting and
//! `--simplify-merges`.
//!
//! Written from the "History Simplification" section of
//! `Documentation/rev-list-options.adoc` and from the observable output of
//! the `git` binary.
//!
//! A commit is TREESAME to a parent when a diff limited to the paths of
//! interest shows no change ([`CommitSource::treesame`]); a root commit is
//! compared to the empty tree.
//!
//! - Default mode: when a commit is TREESAME to one of its parents, only
//!   that parent is walked (the first such parent) and the commit's parent
//!   list shrinks to it. Otherwise all parents are walked.
//! - `--full-history`: all parents are walked. A merge is TREESAME when it
//!   is TREESAME to all of its *relevant* parents: interesting parents and
//!   excluded tips (bottoms), but with `--ancestry-path` only those on the
//!   path. Without relevant parents every parent counts.
//! - TREESAME commits are hidden, unless `--sparse`.
//! - Parent rewriting replaces every parent of a shown commit by the first
//!   commit along its line that is shown, excluded or off the ancestry
//!   path; parents that lead to nothing (a hidden root) are dropped. With
//!   `--full-history` merges with at least two relevant parents are shown.
//! - `--simplify-merges` simplifies the rewritten graph further, see
//!   [`Walk::simplify_merges`].

use std::collections::{HashMap, HashSet};

use rgit_hash::ObjectId;

use super::{
    ADDED, BOTTOM, CommitSource, DROPPED, KEEP, PULL, RELATED, SEEN, SIMPLIFIED, TREESAME,
    TraverseFailure, UNINTERESTING, Walk,
};

impl<S: CommitSource> Walk<S> {
    /// Compare an interesting commit with its parents when the walk takes
    /// it out of the queue; in the default mode keep only the first
    /// TREESAME parent.
    pub(super) fn simplify_parents(&mut self, n: u32) -> Result<(), S::Error> {
        self.nodes[n as usize].flags |= SIMPLIFIED;
        let id = self.nodes[n as usize].id;
        let parents = self.nodes[n as usize].parents.clone();
        if parents.is_empty() {
            if self.source.treesame(&id, None)? {
                self.nodes[n as usize].flags |= TREESAME;
            }
            return Ok(());
        }
        let count = if self.opts.first_parent {
            1
        } else {
            parents.len()
        };
        let mut keep = Vec::with_capacity(parents.len());
        let mut same = Vec::with_capacity(parents.len());
        for (i, p) in parents.iter().enumerate() {
            if i >= count {
                keep.push(*p);
                same.push(false);
                continue;
            }
            if let Err(e) = self.node(p) {
                self.traverse_failure = Some(TraverseFailure::Simplify {
                    commit: id,
                    parent: *p,
                });
                return Err(e);
            }
            let ts = self.source.treesame(&id, Some(p))?;
            // An excluded parent that is not a bottom only counts when no
            // parent is relevant (observed with merges of side branches
            // that start below the excluded tip).
            let counts = self.relevant(p) || !parents[..count].iter().any(|q| self.relevant(q));
            if ts && counts && !self.opts.full_history {
                let node = &mut self.nodes[n as usize];
                node.flags |= TREESAME;
                if i > 0 && self.opts.show_pulls {
                    node.flags |= PULL;
                }
                node.orig_parents = Some(parents.clone());
                node.parents = vec![*p];
                node.same = vec![true];
                return Ok(());
            }
            if !ts && self.opts.remove_empty && self.source.only_added(&id, p)? {
                // Not walked, but the commit still differs from it.
                self.nodes[n as usize].flags |= ADDED;
                continue;
            }
            keep.push(*p);
            same.push(ts);
        }
        let node = &mut self.nodes[n as usize];
        let walked = &same[..count.min(same.len())];
        let treesame = if keep.is_empty() {
            // Every parent lost the paths: like a root commit.
            self.source.treesame(&id, None)?
        } else {
            node.flags & ADDED == 0 && !walked.is_empty() && walked.iter().all(|&s| s)
        };
        let node_flags = &mut node.flags;
        if treesame {
            *node_flags |= TREESAME;
        }
        if self.opts.show_pulls && walked.len() > 1 && !walked[0] && walked[1..].iter().any(|&s| s)
        {
            *node_flags |= PULL;
        }
        if keep.len() != parents.len() {
            node.orig_parents = Some(parents);
        }
        node.parents = keep;
        node.same = same;
        Ok(())
    }

    /// Parent rewriting during a limited walk: follow each parent of `n`
    /// down the commits that will be simplified away and put their parents
    /// in the queue right away, as the reference implementation does
    /// (this decides the order of the result).
    pub(super) fn prewalk(&mut self, n: u32) -> Result<(), S::Error> {
        // Every parent is rewritten, also with `--first-parent`; but then
        // a later parent is looked at only when the walk already met it.
        for i in 0..self.nodes[n as usize].parents.len() {
            let mut p = self.nodes[n as usize].parents[i];
            if i > 0
                && self.opts.first_parent
                && self
                    .index
                    .get(&p)
                    .is_none_or(|&pn| self.nodes[pn as usize].flags & SEEN == 0)
            {
                continue;
            }
            loop {
                let pn = self.node(&p)?;
                if self.uninteresting(pn) || self.too_old(pn) {
                    break;
                }
                self.expand(pn)?;
                let node = &self.nodes[pn as usize];
                if self.shown_now(pn) || node.parents.len() != 1 {
                    break;
                }
                p = node.parents[0];
            }
        }
        Ok(())
    }

    /// Shown as far as the walk knows yet (merges count as shown with
    /// `--full-history`).
    pub(super) fn shown_now(&self, n: u32) -> bool {
        let node = &self.nodes[n as usize];
        self.opts.sparse
            || node.flags & (TREESAME | PULL) != TREESAME
            || (self.opts.full_history && node.parents.len() > 1)
    }

    /// Hidden by history simplification.
    pub(super) fn simplified_away(&self, n: u32) -> bool {
        if !self.opts.prune || self.opts.sparse {
            return false;
        }
        let f = self.nodes[n as usize].flags;
        f & DROPPED != 0 || (f & TREESAME != 0 && f & (PULL | KEEP) == 0)
    }

    /// A parent that counts when deciding whether a merge is TREESAME.
    fn relevant(&self, p: &ObjectId) -> bool {
        let Some(&pn) = self.index.get(p) else {
            return true;
        };
        let f = self.nodes[pn as usize].flags;
        if f & UNINTERESTING != 0 {
            return f & BOTTOM != 0;
        }
        !self.opts.ancestry_path || f & RELATED != 0
    }

    /// Off the ancestry path: such commits end parent rewriting like
    /// excluded ones.
    fn off_path(&self, n: u32) -> bool {
        self.opts.ancestry_path && self.nodes[n as usize].flags & RELATED == 0
    }

    /// After a limited walk, before sorting: settle TREESAME of merges.
    pub(super) fn settle(&mut self, list: &[u32]) {
        if self.opts.full_history {
            for &n in list {
                self.settle_merge(n);
            }
        }
    }

    /// After sorting: rewrite parents or run `--simplify-merges`. Commits
    /// simplified away stay in `list`; [`Walk::simplified_away`] hides
    /// them.
    pub(super) fn rewrite_all(&mut self, list: &[u32]) {
        if self.opts.simplify_merges {
            self.simplify_merges(list);
        } else if self.opts.rewrite_parents {
            for &n in list {
                if !self.simplified_away(n) {
                    self.rewrite(n);
                }
            }
        }
    }

    /// `--full-history`: a merge is TREESAME when it is TREESAME to all
    /// relevant parents (all parents when none is relevant). With parent
    /// rewriting, merges with two or more relevant parents are shown.
    fn settle_merge(&mut self, n: u32) {
        let node = &self.nodes[n as usize];
        if node.flags & SIMPLIFIED == 0 || node.parents.len() < 2 {
            return;
        }
        let count = if self.opts.first_parent {
            1
        } else {
            node.parents.len()
        };
        let relevant: Vec<usize> = (0..count)
            .filter(|&i| self.relevant(&node.parents[i]))
            .collect();
        let treesame = if node.flags & ADDED != 0 {
            false
        } else if relevant.is_empty() {
            node.same[..count].iter().all(|&s| s)
        } else {
            relevant.iter().all(|&i| node.same[i])
        };
        let keep = self.opts.rewrite_parents && relevant.len() >= 2;
        let node = &mut self.nodes[n as usize];
        node.flags &= !(TREESAME | KEEP);
        if treesame {
            node.flags |= TREESAME;
        }
        if keep {
            node.flags |= KEEP;
        }
    }

    /// The commit that stands for parent `p` of a shown commit: `p` itself
    /// when it is shown, excluded or off the ancestry path, else (for a
    /// hidden commit) what its parent stands for. `None` when the line ends
    /// in a hidden root.
    fn rewrite_one(&self, mut p: ObjectId) -> Option<ObjectId> {
        let mut steps = 0usize;
        loop {
            let Some(&pn) = self.index.get(&p) else {
                return Some(p);
            };
            let node = &self.nodes[pn as usize];
            if node.flags & UNINTERESTING != 0
                || node.flags & SIMPLIFIED == 0
                || self.off_path(pn)
                || self.opts.sparse
                || node.flags & (TREESAME | PULL) != TREESAME
            {
                return Some(p);
            }
            // A TREESAME commit. Shown merges are rewritten in output
            // order, in place: one whose parents all vanished counts as a
            // hidden root, one left with a single parent is passed through.
            p = match node.parents.len() {
                0 => return None,
                1 => node.parents[0],
                _ if node.flags & KEEP != 0 => return Some(p),
                _ => node.parents[self.hidden_merge_pick(pn)],
            };
            steps += 1;
            if steps > self.nodes.len() {
                return Some(p);
            }
        }
    }

    /// The parent a hidden merge stands for: the first relevant parent it
    /// is TREESAME to, else the first relevant one, else the first.
    fn hidden_merge_pick(&self, n: u32) -> usize {
        let node = &self.nodes[n as usize];
        let len = node.parents.len();
        let same = |i: usize| node.same.get(i).copied().unwrap_or(false);
        (0..len)
            .find(|&i| same(i) && self.relevant(&node.parents[i]))
            .or_else(|| (0..len).find(|&i| self.relevant(&node.parents[i])))
            .unwrap_or(0)
    }

    /// Rewrite the parents of `n` and remove duplicates.
    fn rewrite(&mut self, n: u32) {
        let parents = self.nodes[n as usize].parents.clone();
        let same = self.nodes[n as usize].same.clone();
        let mut out: Vec<ObjectId> = Vec::with_capacity(parents.len());
        let mut out_same = Vec::with_capacity(parents.len());
        for (i, p) in parents.iter().enumerate() {
            let s = same.get(i).copied().unwrap_or(false);
            let Some(r) = self.rewrite_one(*p) else {
                continue;
            };
            match out.iter().position(|o| *o == r) {
                Some(j) => out_same[j] |= s,
                None => {
                    out.push(r);
                    out_same.push(s);
                }
            }
        }
        if out != parents {
            let node = &mut self.nodes[n as usize];
            if node.orig_parents.is_none() {
                node.orig_parents = Some(parents);
            }
            node.parents = out;
            node.same = out_same;
        }
    }

    /// `--simplify-merges`: starting from the graph `--full-history` with
    /// parent rewriting shows, replace each commit `C` by its
    /// simplification: its parents are replaced by theirs, parents that
    /// are ancestors of other parents, hidden roots and duplicates are
    /// dropped (keeping at least one parent `C` is TREESAME to). A commit
    /// that then has exactly one parent, is not excluded and is TREESAME
    /// (and not a pull) is replaced by that parent; the others remain.
    /// The result comes in topological order.
    fn simplify_merges(&mut self, list: &[u32]) {
        // Parents before children.
        let order = self.sort_topologically(list, super::Order::Topo, None);
        let in_list: HashSet<u32> = list.iter().copied().collect();
        // Commit -> what it is replaced by (None: removed).
        let mut simp: HashMap<u32, Option<ObjectId>> = HashMap::new();
        for &n in order.iter().rev() {
            let node = &self.nodes[n as usize];
            if node.flags & SIMPLIFIED == 0 {
                simp.insert(n, Some(node.id));
                continue;
            }
            let mut parents = node.parents.clone();
            let mut same = node.same.clone();
            if parents.len() > 1 && self.simplified_away(n) {
                // Hidden in the rewritten graph: it stands for the parent
                // parent rewriting would follow.
                let pick = self.hidden_merge_pick(n);
                parents = vec![parents[pick]];
                same = vec![same.get(pick).copied().unwrap_or(false)];
            }
            let mut cand: Vec<(ObjectId, bool)> = Vec::new();
            for (i, p) in parents.iter().enumerate() {
                let s = same.get(i).copied().unwrap_or(false);
                let r = match self.index.get(p) {
                    Some(pn) if in_list.contains(pn) && !self.off_path(*pn) => {
                        simp.get(pn).copied().unwrap_or(Some(*p))
                    }
                    _ => Some(*p),
                };
                let Some(r) = r else {
                    continue;
                };
                // Roots TREESAME to the empty tree are dropped (with
                // `--sparse` they are still shown).
                if let Some(&rn) = self.index.get(&r) {
                    let f = self.nodes[rn as usize].flags;
                    if in_list.contains(&rn)
                        && f & (TREESAME | UNINTERESTING | SIMPLIFIED) == TREESAME | SIMPLIFIED
                        && self.nodes[rn as usize].parents.is_empty()
                    {
                        continue;
                    }
                }
                match cand.iter_mut().find(|c| c.0 == r) {
                    Some(c) => c.1 |= s,
                    None => cand.push((r, s)),
                }
            }
            // Drop parents that are ancestors of other parents.
            let mut drop = vec![false; cand.len()];
            if cand.len() > 1 {
                for i in 0..cand.len() {
                    for j in 0..cand.len() {
                        if i != j && !drop[j] && self.is_ancestor(&cand[i].0, &cand[j].0) {
                            drop[i] = true;
                            break;
                        }
                    }
                }
                let had_same = cand.iter().any(|c| c.1);
                let kept_same = cand.iter().zip(&drop).any(|(c, d)| c.1 && !d);
                if had_same
                    && !kept_same
                    && let Some(i) = cand.iter().position(|c| c.1)
                {
                    drop[i] = false;
                }
            }
            let kept: Vec<(ObjectId, bool)> = cand
                .into_iter()
                .zip(drop)
                .filter(|(_, d)| !d)
                .map(|(c, _)| c)
                .collect();
            let flags = self.nodes[n as usize].flags;
            // A merge with one relevant parent left counts as having only
            // that parent.
            let relevant: Vec<&(ObjectId, bool)> =
                kept.iter().filter(|c| self.relevant(&c.0)).collect();
            let single = match (kept.len(), relevant.len()) {
                (1, _) => Some(kept[0]),
                (k, 1) if k > 1 => Some(*relevant[0]),
                _ => None,
            };
            let treesame = match (kept.len(), single) {
                (0, _) => flags & TREESAME != 0,
                (_, Some(c)) => c.1,
                _ => false,
            };
            // `--sparse` keeps every commit.
            let pull = flags & PULL != 0 || self.opts.sparse;
            let node = &mut self.nodes[n as usize];
            let new_parents: Vec<ObjectId> = kept.iter().map(|c| c.0).collect();
            if new_parents != node.parents {
                if node.orig_parents.is_none() {
                    node.orig_parents = Some(node.parents.clone());
                }
                node.parents = new_parents;
                node.same = kept.iter().map(|c| c.1).collect();
            }
            node.flags &= !(TREESAME | KEEP | DROPPED);
            let id = node.id;
            if kept.is_empty() {
                if treesame && !pull {
                    node.flags |= TREESAME | DROPPED;
                    simp.insert(n, None);
                } else {
                    simp.insert(n, Some(id));
                }
            } else if let Some(c) = single.filter(|_| treesame && !pull) {
                node.flags |= TREESAME | DROPPED;
                simp.insert(n, Some(c.0));
            } else {
                if treesame {
                    node.flags |= TREESAME | KEEP;
                }
                simp.insert(n, Some(id));
            }
        }
    }

    /// `a` is an ancestor of `b` in the simplified graph.
    fn is_ancestor(&self, a: &ObjectId, b: &ObjectId) -> bool {
        let mut stack = vec![*b];
        let mut seen = HashSet::new();
        while let Some(c) = stack.pop() {
            if c == *a {
                return true;
            }
            let Some(&cn) = self.index.get(&c) else {
                continue;
            };
            if seen.insert(cn) {
                stack.extend(self.nodes[cn as usize].parents.iter().copied());
            }
        }
        false
    }
}

#[cfg(test)]
#[path = "simplify_tests.rs"]
mod tests;
