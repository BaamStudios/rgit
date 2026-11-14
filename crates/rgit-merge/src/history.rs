// SPDX-License-Identifier: MIT

//! Merge bases and the recursive merge of several of them into a virtual
//! base (merge-strategies(7): "creates a merged tree of the common
//! ancestors and uses that as the reference tree").
//!
//! The bases are found by painting down from both commits
//! (`Documentation/technical/paint-down-to-common.adoc`) and listed newest
//! first, like `git merge-base --all`. The merge takes them oldest first:
//! the first two are merged, the result becomes a virtual commit whose
//! parents are the two, which is merged with the next base, and so on.
//! Those inner merges compute their own merge bases, which may include
//! virtual commits' ancestry.

use std::collections::{BinaryHeap, HashMap, HashSet};

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind};

use crate::engine::{self, Labels};
use crate::{Error, MergeResult, Objects, Options, messages};

/// What the merge needs to know about a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub tree: ObjectId,
    pub parents: Vec<ObjectId>,
    /// Committer time (seconds since the epoch); orders merge bases.
    pub time: i64,
}

/// Commit access for [`crate::merge_commits`].
pub trait Commits {
    /// The commit `id`.
    ///
    /// # Errors
    /// When it cannot be read or is not a commit.
    fn commit(&self, id: &ObjectId) -> Result<CommitInfo, Error>;

    /// The label of a single merge base after `|||||||` (git uses the
    /// unique abbreviation of its id).
    fn label(&self, id: &ObjectId) -> Vec<u8>;
}

/// Reads commit `id` from `objects`.
///
/// # Errors
/// [`Error::MissingObject`], [`Error::NotACommit`] or [`Error::Corrupt`].
pub fn read_commit(objects: &dyn Objects, id: &ObjectId) -> Result<CommitInfo, Error> {
    let raw = match objects.read(id).map_err(Error::Read)? {
        Some(raw) if raw.kind == ObjectKind::Commit => raw,
        Some(_) => return Err(Error::NotACommit(*id)),
        None => return Err(Error::MissingObject(*id)),
    };
    let corrupt = |e: rgit_object::Error| Error::Corrupt(*id, e.to_string());
    let c = Commit::parse(objects.hash_kind(), &raw.data).map_err(corrupt)?;
    Ok(CommitInfo {
        tree: c.tree().map_err(corrupt)?,
        parents: c.parents().map_err(corrupt)?,
        time: c
            .committer()
            .and_then(|s| s.time())
            .map_or(0, |t| t.seconds),
    })
}

/// Names a merge base for the conflict markers.
pub type LabelFn<'a> = Box<dyn Fn(&ObjectId) -> Vec<u8> + 'a>;

/// [`Commits`] read from an object database.
pub struct ObjectCommits<'a> {
    pub objects: &'a dyn Objects,
    /// Commits whose parents are cut off (`.git/shallow`).
    pub shallow: HashSet<ObjectId>,
    /// The label of a merge base.
    pub label: LabelFn<'a>,
}

impl Commits for ObjectCommits<'_> {
    fn commit(&self, id: &ObjectId) -> Result<CommitInfo, Error> {
        let mut c = read_commit(self.objects, id)?;
        if self.shallow.contains(id) {
            c.parents.clear();
        }
        Ok(c)
    }

    fn label(&self, id: &ObjectId) -> Vec<u8> {
        (self.label)(id)
    }
}

/// The merge bases of `a` and `b`, newest first (`git merge-base --all`).
///
/// # Errors
/// When a commit cannot be read.
pub fn merge_bases(
    commits: &dyn Commits,
    a: &ObjectId,
    b: &ObjectId,
) -> Result<Vec<ObjectId>, Error> {
    let mut g = Graph::new(commits);
    let bases = g.merge_bases(Node::Real(*a), Node::Real(*b))?;
    Ok(bases
        .into_iter()
        .filter_map(|n| match n {
            Node::Real(id) => Some(id),
            Node::Virtual(_) => None,
        })
        .collect())
}

/// A commit of the merge: a real one, or a virtual merge of merge bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Node {
    Real(ObjectId),
    Virtual(usize),
}

struct NodeInfo {
    tree: ObjectId,
    parents: Vec<Node>,
    time: i64,
}

struct Graph<'a> {
    commits: &'a dyn Commits,
    real: HashMap<ObjectId, NodeInfo>,
    virtuals: Vec<NodeInfo>,
}

impl<'a> Graph<'a> {
    fn new(commits: &'a dyn Commits) -> Graph<'a> {
        Graph {
            commits,
            real: HashMap::new(),
            virtuals: Vec::new(),
        }
    }

    fn info(&mut self, n: Node) -> Result<&NodeInfo, Error> {
        match n {
            Node::Virtual(i) => Ok(&self.virtuals[i]),
            Node::Real(id) => {
                if !self.real.contains_key(&id) {
                    let c = self.commits.commit(&id)?;
                    self.real.insert(
                        id,
                        NodeInfo {
                            tree: c.tree,
                            parents: c.parents.into_iter().map(Node::Real).collect(),
                            time: c.time,
                        },
                    );
                }
                Ok(&self.real[&id])
            }
        }
    }

    /// A virtual commit (time 0, like git's) with the given tree and
    /// parents.
    fn add_virtual(&mut self, tree: ObjectId, parents: Vec<Node>) -> Node {
        self.virtuals.push(NodeInfo {
            tree,
            parents,
            time: 0,
        });
        Node::Virtual(self.virtuals.len() - 1)
    }

    /// The best common ancestors of `a` and `b`, newest first.
    fn merge_bases(&mut self, a: Node, b: Node) -> Result<Vec<Node>, Error> {
        if a == b {
            return Ok(vec![a]);
        }
        let candidates = self.paint_down(a, &[b])?;
        let mut bases = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            let others: Vec<Node> = candidates
                .iter()
                .enumerate()
                .filter(|&(j, o)| j != i && o != c)
                .map(|(_, o)| *o)
                .collect();
            if !others.is_empty() && self.paint_down(*c, &others)?.contains(c) {
                // An ancestor of another candidate.
                continue;
            }
            bases.push(*c);
        }
        let mut timed = Vec::with_capacity(bases.len());
        for n in bases {
            timed.push((self.info(n)?.time, n));
        }
        timed.sort_by_key(|x| std::cmp::Reverse(x.0));
        Ok(timed.into_iter().map(|(_, n)| n).collect())
    }

    /// Common ancestors of `one` and `twos` not reachable from another
    /// common ancestor found during the walk: a date-ordered queue,
    /// commits painted from either side, common ones recorded and their
    /// ancestors painted stale; the walk ends when only stale commits are
    /// queued.
    fn paint_down(&mut self, one: Node, twos: &[Node]) -> Result<Vec<Node>, Error> {
        #[derive(Default)]
        struct Paint {
            flags: u8,
            /// Queue entries of this commit.
            queued: usize,
        }
        struct Walk {
            paint: HashMap<Node, Paint>,
            // (time, insertion counter): newest first, then first queued.
            heap: BinaryHeap<(i64, u64, Node)>,
            counter: u64,
            /// Queue entries whose commit is not stale.
            active: usize,
        }
        impl Walk {
            fn queue(&mut self, n: Node, f: u8, time: i64) {
                let p = self.paint.entry(n).or_default();
                if p.flags & f == f {
                    return;
                }
                if f & STALE != 0 && p.flags & STALE == 0 {
                    self.active -= p.queued;
                }
                p.flags |= f;
                p.queued += 1;
                if p.flags & STALE == 0 {
                    self.active += 1;
                }
                self.counter -= 1;
                self.heap.push((time, self.counter, n));
            }
        }
        const P1: u8 = 1;
        const P2: u8 = 2;
        const STALE: u8 = 4;
        const RESULT: u8 = 8;
        let mut w = Walk {
            paint: HashMap::new(),
            heap: BinaryHeap::new(),
            counter: u64::MAX,
            active: 0,
        };
        let time = self.info(one)?.time;
        w.queue(one, P1, time);
        for t in twos {
            let time = self.info(*t)?.time;
            w.queue(*t, P2, time);
        }
        let mut result = Vec::new();
        while w.active > 0 {
            let Some((_, _, n)) = w.heap.pop() else { break };
            let p = w.paint.get_mut(&n).expect("queued commits are painted");
            p.queued -= 1;
            if p.flags & STALE == 0 {
                w.active -= 1;
            }
            let f = p.flags;
            let mut pf = f & (P1 | P2 | STALE);
            if f & (P1 | P2) == (P1 | P2) {
                if f & (RESULT | STALE) == 0 {
                    p.flags |= RESULT;
                    result.push(n);
                }
                pf |= STALE;
            }
            let parents = self.info(n)?.parents.clone();
            for parent in parents {
                let time = self.info(parent)?.time;
                w.queue(parent, pf, time);
            }
        }
        result.retain(|n| w.paint[n].flags & STALE == 0);
        Ok(result)
    }
}

/// The recursive merge of commits `h1` (ours) and `h2` (theirs).
pub(crate) fn merge_recursive(
    objects: &dyn Objects,
    commits: &dyn Commits,
    ours: &ObjectId,
    theirs: &ObjectId,
    bases: Option<&[ObjectId]>,
    opts: &Options,
) -> Result<MergeResult, Error> {
    let mut g = Graph::new(commits);
    let bases = bases.map(|b| b.iter().map(|id| Node::Real(*id)).collect());
    merge_nodes(
        objects,
        &mut g,
        opts,
        Node::Real(*ours),
        Node::Real(*theirs),
        bases,
        0,
    )
}

fn merge_nodes(
    objects: &dyn Objects,
    g: &mut Graph<'_>,
    opts: &Options,
    h1: Node,
    h2: Node,
    bases: Option<Vec<Node>>,
    depth: usize,
) -> Result<MergeResult, Error> {
    let mut bases = match bases {
        Some(b) => b,
        None => g.merge_bases(h1, h2)?,
    };
    // Oldest first.
    bases.reverse();
    let (base_tree, ancestor) = match bases.as_slice() {
        [] => {
            if depth == 0 && !opts.allow_unrelated_histories {
                return Err(Error::UnrelatedHistories);
            }
            (
                objects.hash_kind().empty_tree(),
                messages::EMPTY_TREE.as_bytes().to_vec(),
            )
        }
        [only] => {
            let label = match only {
                Node::Real(id) => g.commits.label(id),
                Node::Virtual(_) => messages::MERGED_COMMON_ANCESTORS.as_bytes().to_vec(),
            };
            (g.info(*only)?.tree, label)
        }
        [first, rest @ ..] => {
            let mut merged = *first;
            for next in rest {
                let inner = merge_nodes(objects, g, opts, merged, *next, None, depth + 1)?;
                merged = g.add_virtual(inner.tree, vec![merged, *next]);
            }
            (
                g.info(merged)?.tree,
                messages::MERGED_COMMON_ANCESTORS.as_bytes().to_vec(),
            )
        }
    };
    let ours_tree = g.info(h1)?.tree;
    let theirs_tree = g.info(h2)?.tree;
    let labels = if depth == 0 {
        Labels {
            ours: &opts.branch1,
            theirs: &opts.branch2,
            base: &ancestor,
        }
    } else {
        Labels {
            ours: messages::TEMPORARY_BRANCH_1.as_bytes(),
            theirs: messages::TEMPORARY_BRANCH_2.as_bytes(),
            base: &ancestor,
        }
    };
    engine::merge(
        objects,
        opts,
        labels,
        depth,
        &base_tree,
        &ours_tree,
        &theirs_tree,
    )
}
