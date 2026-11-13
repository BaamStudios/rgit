// SPDX-License-Identifier: MIT
//! Reachability queries between commits (Documentation/git-merge-base.adoc):
//! merge bases of several commits, octopus merge bases, independent
//! commits, is-ancestor, and ahead/behind counts.
//!
//! All functions take commit ids (callers peel tags first). Results do not
//! depend on commit dates being monotonic: dates only order the walks.

use std::collections::{BinaryHeap, HashMap};

use rgit_hash::ObjectId;

use super::{Error, Resolver};

impl Resolver<'_> {
    /// The best common ancestors of `one` and any of `twos`
    /// (`git merge-base --all <one> <two>...`): the merge bases of `one`
    /// and a hypothetical merge of all `twos`. Newest commit first, like
    /// git prints them.
    pub fn merge_bases_many(
        &self,
        one: &ObjectId,
        twos: &[ObjectId],
    ) -> Result<Vec<ObjectId>, Error> {
        if twos.is_empty() {
            return Ok(Vec::new());
        }
        let candidates = self.paint_down(one, twos)?;
        let bases = self.remove_redundant(candidates)?;
        self.sort_newest_first(bases)
    }

    /// The best common ancestors of all `commits` for an n-way merge
    /// (`git merge-base --octopus --all`), newest first.
    pub fn octopus_merge_bases(&self, commits: &[ObjectId]) -> Result<Vec<ObjectId>, Error> {
        let Some((first, rest)) = commits.split_first() else {
            return Ok(Vec::new());
        };
        let mut result = vec![*first];
        for c in rest {
            let mut next: Vec<ObjectId> = Vec::new();
            for r in &result {
                for b in self.merge_bases_many(r, std::slice::from_ref(c))? {
                    if !next.contains(&b) {
                        next.push(b);
                    }
                }
            }
            result = next;
            if result.is_empty() {
                break;
            }
        }
        let result = self.remove_redundant(result)?;
        self.sort_newest_first(result)
    }

    /// The `commits` not reachable from any other of them
    /// (`git merge-base --independent`), in input order without
    /// duplicates.
    pub fn independent(&self, commits: &[ObjectId]) -> Result<Vec<ObjectId>, Error> {
        let mut unique: Vec<ObjectId> = Vec::new();
        for c in commits {
            if !unique.contains(c) {
                unique.push(*c);
            }
        }
        self.remove_redundant(unique)
    }

    /// Whether `ancestor` is reachable from (or equal to) `descendant`
    /// (`git merge-base --is-ancestor`).
    pub fn is_ancestor(&self, ancestor: &ObjectId, descendant: &ObjectId) -> Result<bool, Error> {
        self.is_ancestor_of_any(ancestor, std::slice::from_ref(descendant))
    }

    /// How many commits are reachable from `a` but not from `b` (ahead) and
    /// from `b` but not from `a` (behind): `git rev-list --left-right
    /// --count a...b`.
    ///
    /// Both sides are walked newest first and painted LEFT and RIGHT;
    /// commits with both colours pass both colours on to their parents.
    /// The walk stops once only doubly painted commits are queued; then the
    /// double colour is still carried into already visited commits, so the
    /// counts are exact even when commit dates are skewed.
    pub fn ahead_behind(&self, a: &ObjectId, b: &ObjectId) -> Result<(usize, usize), Error> {
        const LEFT: u8 = 1;
        const RIGHT: u8 = 2;
        const BOTH: u8 = LEFT | RIGHT;
        struct Node {
            flags: u8,
            time: u64,
            parents: Vec<ObjectId>,
            queued: bool,
        }
        struct State {
            nodes: HashMap<ObjectId, Node>,
            heap: BinaryHeap<(u64, u64, ObjectId)>,
            counter: u64,
            /// Queued commits that are not painted BOTH.
            single: usize,
        }
        impl State {
            fn mark(&mut self, r: &Resolver<'_>, oid: ObjectId, f: u8) -> Result<(), Error> {
                if let std::collections::hash_map::Entry::Vacant(e) = self.nodes.entry(oid) {
                    let (time, parents) = match r.commit_info(&oid)? {
                        Some(c) => (c.time, c.parents),
                        None => (0, Vec::new()),
                    };
                    e.insert(Node {
                        flags: 0,
                        time,
                        parents,
                        queued: false,
                    });
                }
                let n = self.nodes.get_mut(&oid).expect("inserted");
                let old = n.flags;
                let new = old | f;
                if new == old {
                    return Ok(());
                }
                n.flags = new;
                if n.queued {
                    if new == BOTH {
                        self.single -= 1;
                    }
                } else {
                    n.queued = true;
                    self.counter -= 1;
                    self.heap.push((n.time, self.counter, oid));
                    if new != BOTH {
                        self.single += 1;
                    }
                }
                Ok(())
            }
        }
        if a == b {
            return Ok((0, 0));
        }
        let mut s = State {
            nodes: HashMap::new(),
            heap: BinaryHeap::new(),
            counter: u64::MAX,
            single: 0,
        };
        s.mark(self, *a, LEFT)?;
        s.mark(self, *b, RIGHT)?;
        while let Some((_, _, oid)) = s.heap.pop() {
            let n = s.nodes.get_mut(&oid).expect("queued commits are known");
            n.queued = false;
            let f = n.flags;
            if f != BOTH {
                s.single -= 1;
            }
            let draining = s.single == 0 && f == BOTH;
            let parents = std::mem::take(&mut n.parents);
            for p in &parents {
                // Once no single colour is queued, it can never reach an
                // unvisited commit again: only visited ones need BOTH.
                if draining && s.nodes.get(p).is_none_or(|n| n.flags == BOTH) {
                    continue;
                }
                s.mark(self, *p, f)?;
            }
            s.nodes.get_mut(&oid).expect("known").parents = parents;
        }
        let mut ahead = 0;
        let mut behind = 0;
        for n in s.nodes.values() {
            match n.flags {
                LEFT => ahead += 1,
                RIGHT => behind += 1,
                _ => {}
            }
        }
        Ok((ahead, behind))
    }

    /// Order commits newest first (by committer date; ties keep their
    /// order).
    fn sort_newest_first(&self, commits: Vec<ObjectId>) -> Result<Vec<ObjectId>, Error> {
        let mut keyed = Vec::with_capacity(commits.len());
        for c in commits {
            let time = self.commit_info(&c)?.map_or(0, |i| i.time);
            keyed.push((time, c));
        }
        keyed.sort_by_key(|k| std::cmp::Reverse(k.0));
        Ok(keyed.into_iter().map(|(_, c)| c).collect())
    }
}
