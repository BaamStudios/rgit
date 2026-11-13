// SPDX-License-Identifier: MIT
//! Object helpers for the resolver: peeling, parents, merge bases and
//! commit message search.

use std::collections::{BinaryHeap, HashMap, HashSet};

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Tag};

use super::{Error, Level, Resolver, messages};

/// What the resolver needs to know about a commit.
pub(crate) struct CommitInfo {
    pub tree: ObjectId,
    pub parents: Vec<ObjectId>,
    pub time: u64,
    pub message: Vec<u8>,
}

impl Resolver<'_> {
    pub(crate) fn kind_of(&self, oid: &ObjectId) -> Result<Option<ObjectKind>, Error> {
        Ok(self.repo.objects.kind(oid)?)
    }

    /// Parse the commit `oid`; `Ok(None)` if it is missing or not a commit.
    pub(crate) fn commit_info(&self, oid: &ObjectId) -> Result<Option<CommitInfo>, Error> {
        let Some((kind, data)) = self.repo.objects.read(oid)? else {
            return Ok(None);
        };
        if kind != ObjectKind::Commit {
            return Ok(None);
        }
        let hash = self.repo.objects.hash_kind();
        let c = Commit::parse(hash, &data)
            .map_err(|e| Error::Fatal(format!("bad commit {}: {e}", oid.to_hex())))?;
        let tree = c
            .tree()
            .map_err(|e| Error::Fatal(format!("bad commit {}: {e}", oid.to_hex())))?;
        let parents = if self.repo.objects.is_shallow(oid) {
            Vec::new()
        } else {
            c.parents()
                .map_err(|e| Error::Fatal(format!("bad commit {}: {e}", oid.to_hex())))?
        };
        let time = c
            .committer()
            .and_then(|s| s.time())
            .map(|t| t.seconds.max(0) as u64)
            .unwrap_or(0);
        Ok(Some(CommitInfo {
            tree,
            parents,
            time,
            message: c.message().to_vec(),
        }))
    }

    /// The object a tag points to.
    pub(crate) fn tag_target(&self, oid: &ObjectId) -> Result<Option<ObjectId>, Error> {
        let Some((kind, data)) = self.repo.objects.read(oid)? else {
            return Ok(None);
        };
        if kind != ObjectKind::Tag {
            return Ok(None);
        }
        let t = Tag::parse(self.repo.objects.hash_kind(), &data)
            .map_err(|e| Error::Fatal(format!("bad tag {}: {e}", oid.to_hex())))?;
        Ok(t.object().ok())
    }

    /// Follow tags until a non-tag object; `Ok(None)` if something is
    /// missing.
    pub(crate) fn peel_tags(
        &self,
        oid: &ObjectId,
    ) -> Result<Option<(ObjectId, ObjectKind)>, Error> {
        let mut cur = *oid;
        for _ in 0..1000 {
            match self.kind_of(&cur)? {
                None => return Ok(None),
                Some(ObjectKind::Tag) => match self.tag_target(&cur)? {
                    Some(t) => cur = t,
                    None => return Ok(None),
                },
                Some(k) => return Ok(Some((cur, k))),
            }
        }
        Ok(None)
    }

    /// Peel to a commit (git's `lookup_commit_reference`): reports
    /// "object X is a Y, not a commit" for other types, naming `oid` itself
    /// (also in quiet lookups, like git).
    pub(crate) fn peel_to_commit(&self, oid: &ObjectId) -> Result<ObjectId, Error> {
        match self.peel_tags(oid)? {
            None => Err(Error::NotFound),
            Some((c, ObjectKind::Commit)) => Ok(c),
            Some((_, k)) => {
                self.push_diag(
                    Level::Error,
                    messages::not_a_commit(&oid.to_hex(), k.name()),
                );
                Err(Error::NotFound)
            }
        }
    }

    pub(crate) fn parents_of(&self, commit: &ObjectId) -> Result<Vec<ObjectId>, Error> {
        Ok(self
            .commit_info(commit)?
            .map(|c| c.parents)
            .unwrap_or_default())
    }

    /// All merge bases of `a` and `b` (commits), best ones only
    /// (`git merge-base --all`), sorted by id.
    pub fn merge_bases(&self, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>, Error> {
        if a == b {
            return Ok(vec![*a]);
        }
        let candidates = self.paint_down(a, &[*b])?;
        let mut bases = self.remove_redundant(candidates)?;
        bases.sort();
        Ok(bases)
    }

    /// Common ancestors of `one` and `twos` that are not reachable from
    /// another common ancestor found during the walk ("paint down to
    /// common", see `Documentation/technical/paint-down-to-common.adoc`):
    /// a date-ordered queue, commits painted PARENT1/PARENT2, common ones
    /// recorded and their ancestors painted STALE; the walk stops once the
    /// queue holds only stale commits.
    fn paint_down(&self, one: &ObjectId, twos: &[ObjectId]) -> Result<Vec<ObjectId>, Error> {
        const P1: u8 = 1;
        const P2: u8 = 2;
        const STALE: u8 = 4;
        const RESULT: u8 = 8;
        struct Node {
            flags: u8,
            time: u64,
            parents: Option<Vec<ObjectId>>,
            /// Number of queue entries for this commit.
            queued: usize,
        }
        let mut nodes: HashMap<ObjectId, Node> = HashMap::new();
        // (time, insertion counter) keeps the order deterministic.
        let mut heap: BinaryHeap<(u64, u64, ObjectId)> = BinaryHeap::new();
        let mut counter: u64 = u64::MAX;
        // Queue entries whose commit is not STALE (as of pushing).
        let mut active: usize = 0;

        let mut push = |r: &Self,
                        oid: ObjectId,
                        f: u8,
                        nodes: &mut HashMap<ObjectId, Node>,
                        heap: &mut BinaryHeap<(u64, u64, ObjectId)>,
                        active: &mut usize|
         -> Result<(), Error> {
            if let std::collections::hash_map::Entry::Vacant(e) = nodes.entry(oid) {
                let (time, parents) = match r.commit_info(&oid)? {
                    Some(c) => (c.time, c.parents),
                    None => (0, Vec::new()),
                };
                e.insert(Node {
                    flags: 0,
                    time,
                    parents: Some(parents),
                    queued: 0,
                });
            }
            let n = nodes.get_mut(&oid).expect("inserted");
            if n.flags & f == f {
                return Ok(());
            }
            if f & STALE != 0 && n.flags & STALE == 0 {
                // Its queued entries stop counting as interesting.
                *active -= n.queued;
            }
            n.flags |= f;
            counter -= 1;
            heap.push((n.time, counter, oid));
            n.queued += 1;
            if n.flags & STALE == 0 {
                *active += 1;
            }
            Ok(())
        };
        push(self, *one, P1, &mut nodes, &mut heap, &mut active)?;
        for t in twos {
            push(self, *t, P2, &mut nodes, &mut heap, &mut active)?;
        }
        let mut result = Vec::new();
        // `active` counts queue entries whose commit is not STALE.
        while active > 0 {
            let Some((_, _, oid)) = heap.pop() else { break };
            let n = nodes.get_mut(&oid).expect("queued commits are known");
            let f = n.flags;
            n.queued -= 1;
            if f & STALE == 0 {
                active -= 1;
            }
            let mut pf = f & (P1 | P2 | STALE);
            if f & (P1 | P2) == (P1 | P2) {
                if f & (RESULT | STALE) == 0 {
                    n.flags |= RESULT;
                    result.push(oid);
                }
                pf |= STALE;
            }
            let parents = n.parents.take().unwrap_or_default();
            for p in &parents {
                push(self, *p, pf, &mut nodes, &mut heap, &mut active)?;
            }
            if let Some(n) = nodes.get_mut(&oid) {
                n.parents = Some(parents);
            }
        }
        // Drop results that were painted STALE after being recorded.
        result.retain(|o| nodes.get(o).is_some_and(|n| n.flags & STALE == 0));
        Ok(result)
    }

    /// Drop candidates that are ancestors of other candidates.
    fn remove_redundant(&self, candidates: Vec<ObjectId>) -> Result<Vec<ObjectId>, Error> {
        if candidates.len() < 2 {
            return Ok(candidates);
        }
        let mut keep = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            let others: Vec<ObjectId> = candidates
                .iter()
                .enumerate()
                .filter(|(j, o)| *j != i && *o != c)
                .map(|(_, o)| *o)
                .collect();
            if !self.is_ancestor_of_any(c, &others)? {
                keep.push(*c);
            }
        }
        Ok(keep)
    }

    /// Whether `anc` is reachable from (or equal to) any of `descs`: it is
    /// then their best common ancestor, so a bounded paint-down finds it.
    pub fn is_ancestor_of_any(&self, anc: &ObjectId, descs: &[ObjectId]) -> Result<bool, Error> {
        if descs.is_empty() {
            return Ok(false);
        }
        if descs.contains(anc) {
            return Ok(true);
        }
        Ok(self.paint_down(anc, descs)?.contains(anc))
    }

    /// The youngest commit reachable from `starts` whose message matches
    /// `pattern` (`:/<pattern>` and `^{/<pattern>}`); `!-` negates, `!!` is
    /// a literal `!`, any other `!` sequence is reserved.
    pub(crate) fn search_message(
        &self,
        starts: &[ObjectId],
        pattern: &str,
    ) -> Result<ObjectId, Error> {
        let (negate, re_text) = if let Some(rest) = pattern.strip_prefix('!') {
            if let Some(r) = rest.strip_prefix('-') {
                (true, r)
            } else if rest.starts_with('!') {
                (false, rest)
            } else {
                return Err(Error::NotFound);
            }
        } else {
            (false, pattern)
        };
        let re = regex::Regex::new(re_text).map_err(|_| Error::NotFound)?;
        let mut seen = HashSet::new();
        let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
        let mut infos: HashMap<ObjectId, CommitInfo> = HashMap::new();
        for s in starts {
            if !seen.insert(*s) {
                continue;
            }
            if let Some(c) = self.commit_info(s)? {
                heap.push((c.time, *s));
                infos.insert(*s, c);
            }
        }
        while let Some((_, oid)) = heap.pop() {
            let Some(info) = infos.remove(&oid) else {
                continue;
            };
            let text = String::from_utf8_lossy(&info.message);
            if re.is_match(&text) != negate {
                return Ok(oid);
            }
            for p in info.parents {
                if !seen.insert(p) {
                    continue;
                }
                if let Some(c) = self.commit_info(&p)? {
                    heap.push((c.time, p));
                    infos.insert(p, c);
                }
            }
        }
        Err(Error::NotFound)
    }
}
