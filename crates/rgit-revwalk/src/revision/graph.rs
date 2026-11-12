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
        let parents = c
            .parents()
            .map_err(|e| Error::Fatal(format!("bad commit {}: {e}", oid.to_hex())))?;
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
    /// "object X is a Y, not a commit" for other types.
    pub(crate) fn peel_to_commit(&self, oid: &ObjectId, quiet: bool) -> Result<ObjectId, Error> {
        match self.peel_tags(oid)? {
            None => Err(Error::NotFound),
            Some((c, ObjectKind::Commit)) => Ok(c),
            Some((o, k)) => {
                if !quiet {
                    self.push_diag(Level::Error, messages::not_a_commit(&o.to_hex(), k.name()));
                }
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
    /// another common ancestor found during the walk.
    fn paint_down(&self, one: &ObjectId, twos: &[ObjectId]) -> Result<Vec<ObjectId>, Error> {
        const P1: u8 = 1;
        const P2: u8 = 2;
        const STALE: u8 = 4;
        const RESULT: u8 = 8;
        let mut flags: HashMap<ObjectId, u8> = HashMap::new();
        let mut times: HashMap<ObjectId, u64> = HashMap::new();
        let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
        let mut push = |r: &Self,
                        oid: ObjectId,
                        f: u8,
                        flags: &mut HashMap<ObjectId, u8>,
                        heap: &mut BinaryHeap<(u64, ObjectId)>|
         -> Result<(), Error> {
            let e = flags.entry(oid).or_insert(0);
            if *e & f == f {
                return Ok(());
            }
            *e |= f;
            let t = match times.get(&oid) {
                Some(t) => *t,
                None => {
                    let t = r.commit_info(&oid)?.map(|c| c.time).unwrap_or(0);
                    times.insert(oid, t);
                    t
                }
            };
            heap.push((t, oid));
            Ok(())
        };
        push(self, *one, P1, &mut flags, &mut heap)?;
        for t in twos {
            push(self, *t, P2, &mut flags, &mut heap)?;
        }
        let mut result = Vec::new();
        loop {
            let interesting = heap
                .iter()
                .any(|(_, o)| flags.get(o).copied().unwrap_or(0) & STALE == 0);
            if !interesting {
                break;
            }
            let Some((_, oid)) = heap.pop() else { break };
            let f = flags.get(&oid).copied().unwrap_or(0);
            let mut pf = f & (P1 | P2 | STALE);
            if f & (P1 | P2) == (P1 | P2) {
                if f & RESULT == 0 {
                    flags.insert(oid, f | RESULT);
                    result.push(oid);
                }
                pf |= STALE;
            }
            for p in self.parents_of(&oid)? {
                let pfl = flags.get(&p).copied().unwrap_or(0);
                if pfl & pf == pf {
                    continue;
                }
                push(self, p, pf, &mut flags, &mut heap)?;
            }
        }
        Ok(result)
    }

    /// Drop candidates that are ancestors of other candidates.
    fn remove_redundant(&self, candidates: Vec<ObjectId>) -> Result<Vec<ObjectId>, Error> {
        if candidates.len() < 2 {
            return Ok(candidates);
        }
        let mut keep = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            let mut redundant = false;
            for (j, other) in candidates.iter().enumerate() {
                if i != j && self.is_ancestor(c, other)? {
                    redundant = true;
                    break;
                }
            }
            if !redundant {
                keep.push(*c);
            }
        }
        Ok(keep)
    }

    /// Whether `anc` is reachable from `desc` (and not equal to it).
    fn is_ancestor(&self, anc: &ObjectId, desc: &ObjectId) -> Result<bool, Error> {
        if anc == desc {
            return Ok(false);
        }
        let mut seen = HashSet::new();
        let mut stack = vec![*desc];
        while let Some(c) = stack.pop() {
            for p in self.parents_of(&c)? {
                if &p == anc {
                    return Ok(true);
                }
                if seen.insert(p) {
                    stack.push(p);
                }
            }
        }
        Ok(false)
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
