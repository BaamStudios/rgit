// SPDX-License-Identifier: MIT
//! Walking the commit graph like `git rev-list`.
//!
//! Written from `Documentation/rev-list-description.adoc`,
//! `Documentation/rev-list-options.adoc` ("Commit Limiting", "Commit
//! Ordering") and from the observable output of the `git` binary.
//!
//! The walk starts at a set of tips. Tips pushed as *uninteresting*
//! (`^A`, the left side of `A..B`, everything after `--not`) exclude
//! themselves and all their ancestors.
//!
//! # Ordering
//!
//! - [`Order::Default`]: commits come out of a priority queue ordered by
//!   committer date, newest first; commits with the same date come out in
//!   the order they were put in. A commit's parents are put in (in parent
//!   order) when the commit is taken out. Without exclusions the walk is
//!   lazy: `-n 5` reads only a handful of commits.
//! - [`Order::Date`] and [`Order::Topo`]: the walk first collects the whole
//!   result in default order, then sorts it so that no parent comes before
//!   any of its children. `Date` picks among the ready commits by date,
//!   `Topo` continues with the most recently readied commit, which keeps
//!   lines of history together.
//!
//! With exclusions the walk is *limited*: it collects the whole result
//! before showing anything. Excluding a commit excludes its parents right
//! away (the parents of an excluded tip even before they are read), and
//! everything further down as the walk reaches it. The walk stops when only
//! excluded commits are queued and [`SLOP`] more of them have been taken out
//! while the newest queued commit stayed older than the last collected one.
//! Like git, the result can therefore contain commits reachable from an
//! excluded tip when committer dates are badly skewed. Collected commits
//! that turn out to be excluded are dropped.
//!
//! `skip`, `max_count` and then `reverse` apply to the final sequence.
//!
//! Commit data comes from a [`CommitSource`], so the walk works over any
//! object store (and over a fake one in tests).

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use rgit_hash::ObjectId;

/// What the walk needs to know about a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Parent ids in header order (already adjusted for shallow
    /// boundaries and grafts by the source).
    pub parents: Vec<ObjectId>,
    /// Committer time in seconds since the epoch.
    pub time: i64,
}

/// Supplies commits to the walk.
pub trait CommitSource {
    /// Error returned for missing or unreadable commits.
    type Error;
    /// Read the commit `id`.
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, Self::Error>;
}

impl<S: CommitSource + ?Sized> CommitSource for &mut S {
    type Error = S::Error;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, Self::Error> {
        (**self).load(id)
    }
}

/// Output order (`--date-order`, `--topo-order`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Reverse chronological by committer date, as walked.
    #[default]
    Default,
    /// `--date-order`: children before parents, otherwise by date.
    Date,
    /// `--topo-order`: children before parents, lines of history together.
    Topo,
}

/// Options of a walk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WalkOptions {
    /// Output order.
    pub order: Order,
    /// `--reverse`: output the selected commits in reverse order.
    pub reverse: bool,
    /// `-n`/`--max-count`.
    pub max_count: Option<usize>,
    /// `--skip`.
    pub skip: usize,
    /// `--first-parent`: follow only the first parent of included commits.
    pub first_parent: bool,
}

/// Number of extra queue entries the limited walk takes after it believes
/// no excluded commit can reach a collected one any more. Protects against
/// moderate clock skew.
pub const SLOP: usize = 5;

const SEEN: u8 = 1;
const UNINTERESTING: u8 = 2;

#[derive(Debug)]
struct Node {
    id: ObjectId,
    parents: Vec<ObjectId>,
    time: i64,
    flags: u8,
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    time: i64,
    seq: u64,
    node: u32,
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap: newest first, then first inserted first.
        self.time
            .cmp(&other.time)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A commit walk. Push tips with [`Walk::push`], then call
/// [`Walk::next_commit`] until it returns `None`.
pub struct Walk<S: CommitSource> {
    source: S,
    opts: WalkOptions,
    nodes: Vec<Node>,
    index: HashMap<ObjectId, u32>,
    /// Commits marked uninteresting before they were read.
    pending_uninteresting: HashSet<ObjectId>,
    queue: BinaryHeap<Entry>,
    seq: u64,
    limited: bool,
    started: bool,
    prepared: Option<VecDeque<u32>>,
    skipped: usize,
    shown: usize,
}

impl<S: CommitSource> std::fmt::Debug for Walk<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Walk")
            .field("opts", &self.opts)
            .field("commits", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

impl<S: CommitSource> Walk<S> {
    /// A new walk reading commits from `source`.
    pub fn new(source: S, opts: WalkOptions) -> Self {
        Walk {
            source,
            opts,
            nodes: Vec::new(),
            index: HashMap::new(),
            pending_uninteresting: HashSet::new(),
            queue: BinaryHeap::new(),
            seq: 0,
            limited: false,
            started: false,
            prepared: None,
            skipped: 0,
            shown: 0,
        }
    }

    /// The options of this walk.
    pub fn options(&self) -> &WalkOptions {
        &self.opts
    }

    /// The commit source.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// Add a starting point. `uninteresting` excludes the commit and its
    /// ancestors (`^id`). Tips must be pushed before the first
    /// [`Walk::next_commit`]; later pushes are ignored.
    pub fn push(&mut self, id: &ObjectId, uninteresting: bool) -> Result<(), S::Error> {
        if self.started {
            return Ok(());
        }
        let n = self.node(id)?;
        if uninteresting {
            self.limited = true;
            self.mark_uninteresting(n);
            // The parents of an excluded tip are excluded right away, even
            // before the walk reads them.
            for p in &self.nodes[n as usize].parents {
                if !self.index.contains_key(p) {
                    self.pending_uninteresting.insert(*p);
                }
            }
        }
        self.enqueue_once(n);
        Ok(())
    }

    /// Parents of a commit the walk has read, as recorded by the source.
    pub fn parents(&self, id: &ObjectId) -> Option<&[ObjectId]> {
        self.index
            .get(id)
            .map(|&n| self.nodes[n as usize].parents.as_slice())
    }

    /// Committer time of a commit the walk has read.
    pub fn time(&self, id: &ObjectId) -> Option<i64> {
        self.index.get(id).map(|&n| self.nodes[n as usize].time)
    }

    /// The next commit to show, or `None` when the walk is done.
    pub fn next_commit(&mut self) -> Result<Option<ObjectId>, S::Error> {
        if !self.started {
            self.started = true;
            if self.limited || self.opts.order != Order::Default || self.opts.reverse {
                self.prepare()?;
            }
        }
        if let Some(list) = &mut self.prepared {
            return Ok(list.pop_front().map(|n| self.nodes[n as usize].id));
        }
        loop {
            if self.opts.max_count.is_some_and(|m| self.shown >= m) {
                return Ok(None);
            }
            let Some(entry) = self.queue.pop() else {
                return Ok(None);
            };
            self.expand(entry.node)?;
            if self.skipped < self.opts.skip {
                self.skipped += 1;
                continue;
            }
            self.shown += 1;
            return Ok(Some(self.nodes[entry.node as usize].id));
        }
    }

    /// Run the walk to the end and return all commits to show.
    pub fn collect_all(&mut self) -> Result<Vec<ObjectId>, S::Error> {
        let mut out = Vec::new();
        while let Some(id) = self.next_commit()? {
            out.push(id);
        }
        Ok(out)
    }

    fn node(&mut self, id: &ObjectId) -> Result<u32, S::Error> {
        if let Some(&n) = self.index.get(id) {
            return Ok(n);
        }
        let info = self.source.load(id)?;
        let n = u32::try_from(self.nodes.len()).expect("fewer than 2^32 commits");
        let flags = if self.pending_uninteresting.remove(id) {
            UNINTERESTING
        } else {
            0
        };
        self.nodes.push(Node {
            id: *id,
            parents: info.parents,
            time: info.time,
            flags,
        });
        self.index.insert(*id, n);
        Ok(n)
    }

    fn enqueue_once(&mut self, n: u32) {
        let node = &mut self.nodes[n as usize];
        if node.flags & SEEN != 0 {
            return;
        }
        node.flags |= SEEN;
        self.seq += 1;
        self.queue.push(Entry {
            time: node.time,
            seq: self.seq,
            node: n,
        });
    }

    fn is_uninteresting(&self, n: u32) -> bool {
        self.nodes[n as usize].flags & UNINTERESTING != 0
    }

    /// Mark `n` and every ancestor already read as uninteresting. Ancestors
    /// not read yet get the mark when the walk reaches them through the
    /// queue.
    fn mark_uninteresting(&mut self, n: u32) {
        let mut stack = vec![n];
        while let Some(n) = stack.pop() {
            let node = &mut self.nodes[n as usize];
            if node.flags & UNINTERESTING != 0 {
                continue;
            }
            node.flags |= UNINTERESTING;
            for p in &self.nodes[n as usize].parents {
                if let Some(&pn) = self.index.get(p) {
                    stack.push(pn);
                }
            }
        }
    }

    /// Take `n` out of the queue: put its parents in.
    fn expand(&mut self, n: u32) -> Result<(), S::Error> {
        let uninteresting = self.is_uninteresting(n);
        let count = if self.opts.first_parent && !uninteresting {
            self.nodes[n as usize].parents.len().min(1)
        } else {
            self.nodes[n as usize].parents.len()
        };
        for i in 0..count {
            let p = self.nodes[n as usize].parents[i];
            let pn = self.node(&p)?;
            if uninteresting {
                self.mark_uninteresting(pn);
            }
            self.enqueue_once(pn);
        }
        Ok(())
    }

    fn queue_all_uninteresting(&self) -> bool {
        self.queue.iter().all(|e| self.is_uninteresting(e.node))
    }

    /// Collect the whole result, then sort, skip, cut and reverse it.
    fn prepare(&mut self) -> Result<(), S::Error> {
        let mut list = self.limit()?;
        match self.opts.order {
            Order::Default => {}
            Order::Date | Order::Topo => list = self.sort_topologically(&list),
        }
        let mut list: VecDeque<u32> = list
            .into_iter()
            .skip(self.opts.skip)
            .take(self.opts.max_count.unwrap_or(usize::MAX))
            .collect();
        if self.opts.reverse {
            list.make_contiguous().reverse();
        }
        self.prepared = Some(list);
        Ok(())
    }

    /// Walk until no excluded commit can reach a collected one any more.
    ///
    /// The stopping rule follows the observable behaviour of the reference
    /// implementation: once only excluded commits are queued, every further
    /// excluded commit taken out counts down [`SLOP`], unless the newest
    /// queued commit is at least as new as the last collected commit, which
    /// starts the count again.
    fn limit(&mut self) -> Result<Vec<u32>, S::Error> {
        let mut list = Vec::new();
        let mut last_collected: Option<i64> = None;
        let mut slop = SLOP;
        while let Some(entry) = self.queue.pop() {
            let n = entry.node;
            self.expand(n)?;
            if !self.is_uninteresting(n) {
                list.push(n);
                last_collected = Some(self.nodes[n as usize].time);
                continue;
            }
            let Some(head) = self.queue.peek().map(|e| e.time) else {
                break;
            };
            if !self.queue_all_uninteresting() || last_collected.is_some_and(|t| head >= t) {
                slop = SLOP;
                continue;
            }
            slop -= 1;
            if slop == 0 {
                break;
            }
        }
        list.retain(|&n| !self.is_uninteresting(n));
        Ok(list)
    }

    fn sort_topologically(&self, list: &[u32]) -> Vec<u32> {
        // Children still to be shown, per commit; -1 = not in the result.
        let mut indegree = vec![-1i64; self.nodes.len()];
        for &n in list {
            indegree[n as usize] = 0;
        }
        // All parents count, also with `--first-parent`: a commit reached
        // only through first parents still waits for children that merged
        // it as a later parent.
        let parents_of = |n: u32| {
            self.nodes[n as usize]
                .parents
                .iter()
                .filter_map(|p| self.index.get(p).copied())
        };
        for &n in list {
            for p in parents_of(n) {
                if indegree[p as usize] >= 0 {
                    indegree[p as usize] += 1;
                }
            }
        }
        let mut out = Vec::with_capacity(list.len());
        match self.opts.order {
            Order::Topo => {
                let mut stack: Vec<u32> = list
                    .iter()
                    .rev()
                    .copied()
                    .filter(|&n| indegree[n as usize] == 0)
                    .collect();
                while let Some(n) = stack.pop() {
                    out.push(n);
                    for p in parents_of(n) {
                        let d = &mut indegree[p as usize];
                        if *d > 0 {
                            *d -= 1;
                            if *d == 0 {
                                stack.push(p);
                            }
                        }
                    }
                }
            }
            Order::Date | Order::Default => {
                let mut seq = 0u64;
                let mut heap = BinaryHeap::new();
                let mut push = |heap: &mut BinaryHeap<Entry>, n: u32| {
                    seq += 1;
                    heap.push(Entry {
                        time: self.nodes[n as usize].time,
                        seq,
                        node: n,
                    });
                };
                for &n in list {
                    if indegree[n as usize] == 0 {
                        push(&mut heap, n);
                    }
                }
                while let Some(e) = heap.pop() {
                    out.push(e.node);
                    for p in parents_of(e.node) {
                        let d = &mut indegree[p as usize];
                        if *d > 0 {
                            *d -= 1;
                            if *d == 0 {
                                push(&mut heap, p);
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
