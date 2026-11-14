// SPDX-License-Identifier: MIT
//! Walking the commit graph like `git rev-list`.
//!
//! Written from `Documentation/rev-list-description.adoc`,
//! `Documentation/rev-list-options.adoc` ("Commit Limiting", "Commit
//! Ordering", "History Simplification" for `--ancestry-path`) and from the
//! observable output of the `git` binary.
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
//! - [`Order::Date`], [`Order::AuthorDate`] and [`Order::Topo`]: the walk
//!   first collects the whole result in default order, then sorts it so
//!   that no parent comes before any of its children. `Date` and
//!   `AuthorDate` pick among the ready commits by committer or author date,
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
//! # Limiting
//!
//! `--since` ([`WalkOptions::max_age`]) hides commits older than the date
//! and does not walk past them; `--until` ([`WalkOptions::min_age`]), the
//! parent counts, the sides of a symmetric difference and the caller's
//! [`CommitSource::include`] only hide commits. `skip`, `max_count` and
//! then `reverse` apply to what is left.
//!
//! # Boundary
//!
//! With [`WalkOptions::boundary`] the walk ends with the parents of shown
//! commits that were not shown themselves (excluded commits, commits cut
//! off by `-n` or `--since`), marked as [`Shown::boundary`].
//!
//! Commit data comes from a [`CommitSource`], so the walk works over any
//! object store (and over a fake one in tests).

pub mod grep;
pub mod messages;
pub mod objects;

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
    /// The author time of `id`, for [`Order::AuthorDate`]. `None` sorts by
    /// the committer time instead.
    fn author_time(&mut self, id: &ObjectId) -> Result<Option<i64>, Self::Error> {
        let _ = id;
        Ok(None)
    }
    /// Whether `id` passes the caller's own filters (`--grep`, `--author`,
    /// ...). Asked only for commits the walk would show otherwise.
    fn include(&mut self, id: &ObjectId) -> Result<bool, Self::Error> {
        let _ = id;
        Ok(true)
    }
}

impl<S: CommitSource + ?Sized> CommitSource for &mut S {
    type Error = S::Error;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, Self::Error> {
        (**self).load(id)
    }
    fn author_time(&mut self, id: &ObjectId) -> Result<Option<i64>, Self::Error> {
        (**self).author_time(id)
    }
    fn include(&mut self, id: &ObjectId) -> Result<bool, Self::Error> {
        (**self).include(id)
    }
}

/// Output order (`--date-order`, `--author-date-order`, `--topo-order`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Reverse chronological by committer date, as walked.
    #[default]
    Default,
    /// `--date-order`: children before parents, otherwise by date.
    Date,
    /// `--author-date-order`: children before parents, otherwise by
    /// author date.
    AuthorDate,
    /// `--topo-order`: children before parents, lines of history together.
    Topo,
}

/// `--no-walk[=sorted|unsorted]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoWalk {
    /// Newest first by committer date.
    Sorted,
    /// In the order the tips were pushed.
    Unsorted,
}

/// The side of a symmetric difference `A...B` a commit is reachable from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// From `A` (`<` with `--left-right`).
    Left,
    /// From `B` (`>`).
    Right,
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
    /// `--since`/`--max-age`: hide commits older than this time and do
    /// not walk past them.
    pub max_age: Option<i64>,
    /// `--since-as-filter`: hide commits older than this time, but walk
    /// past them.
    pub max_age_filter: Option<i64>,
    /// `--until`/`--min-age`: hide commits newer than this time.
    pub min_age: Option<i64>,
    /// `--min-parents` (`--merges` is 2).
    pub min_parents: Option<usize>,
    /// `--max-parents` (`--no-merges` is 1).
    pub max_parents: Option<usize>,
    /// `--boundary`: end with the boundary commits.
    pub boundary: bool,
    /// `--ancestry-path`: only commits related to the targets (the
    /// excluded tips, or those given with [`Walk::ancestry_target`]).
    pub ancestry_path: bool,
    /// `--left-only`.
    pub left_only: bool,
    /// `--right-only`.
    pub right_only: bool,
    /// `--no-walk`: show the tips only (unless a tip is excluded).
    pub no_walk: Option<NoWalk>,
    /// Record the children of walked commits ([`Walk::children`]).
    pub children: bool,
}

/// One commit of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    /// The commit.
    pub id: ObjectId,
    /// A boundary commit (`--boundary`).
    pub boundary: bool,
    /// The side of the symmetric difference it is reachable from, if any.
    pub side: Option<Side>,
}

/// Number of extra queue entries the limited walk takes after it believes
/// no excluded commit can reach a collected one any more. Protects against
/// moderate clock skew.
pub const SLOP: usize = 5;

const SEEN: u16 = 1;
const UNINTERESTING: u16 = 1 << 1;
const SHOWN: u16 = 1 << 2;
const LEFT: u16 = 1 << 3;
const RIGHT: u16 = 1 << 4;
const BOTTOM: u16 = 1 << 5;
const TARGET: u16 = 1 << 6;
const RELATED: u16 = 1 << 7;
const BOUNDARY: u16 = 1 << 8;
/// Read only to show it as a boundary commit: the walk never reached it,
/// so its parents do not order it.
const UNREACHED: u16 = 1 << 9;

#[derive(Debug)]
struct Node {
    id: ObjectId,
    parents: Vec<ObjectId>,
    time: i64,
    flags: u16,
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
/// [`Walk::next_commit`] (or [`Walk::next_shown`]) until it returns `None`.
pub struct Walk<S: CommitSource> {
    source: S,
    opts: WalkOptions,
    nodes: Vec<Node>,
    index: HashMap<ObjectId, u32>,
    /// Flags given to commits before they were read.
    pending_flags: HashMap<ObjectId, u16>,
    queue: BinaryHeap<Entry>,
    /// Interesting tips in push order (`--no-walk`).
    tips: Vec<u32>,
    seq: u64,
    limited: bool,
    started: bool,
    prepared: Option<VecDeque<u32>>,
    skipped: usize,
    shown: usize,
    /// Commits shown so far, in output order.
    shown_list: Vec<u32>,
    boundary_list: Option<VecDeque<u32>>,
    /// The commits of a limited walk before sorting and filtering.
    limited_list: Vec<u32>,
    children: HashMap<u32, Vec<ObjectId>>,
    /// The whole output of `--reverse --boundary`.
    reversed: Option<VecDeque<Shown>>,
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
            pending_flags: HashMap::new(),
            queue: BinaryHeap::new(),
            tips: Vec::new(),
            seq: 0,
            limited: false,
            started: false,
            prepared: None,
            skipped: 0,
            shown: 0,
            shown_list: Vec::new(),
            boundary_list: None,
            limited_list: Vec::new(),
            children: HashMap::new(),
            reversed: None,
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
        self.push_side(id, uninteresting, None)
    }

    /// Like [`Walk::push`], for a tip of a symmetric difference: the side
    /// is passed on to everything reached from it (`--left-right`).
    pub fn push_side(
        &mut self,
        id: &ObjectId,
        uninteresting: bool,
        side: Option<Side>,
    ) -> Result<(), S::Error> {
        if self.started {
            return Ok(());
        }
        let n = self.node(id)?;
        self.nodes[n as usize].flags |= match side {
            Some(Side::Left) => LEFT,
            Some(Side::Right) => RIGHT,
            None => 0,
        };
        if uninteresting {
            self.limited = true;
            self.nodes[n as usize].flags |= BOTTOM;
            self.mark_uninteresting(n);
            // The parents of an excluded tip are excluded right away, even
            // before the walk reads them.
            for p in self.nodes[n as usize].parents.clone() {
                match self.index.get(&p) {
                    Some(&pn) => self.mark_uninteresting(pn),
                    None => *self.pending_flags.entry(p).or_default() |= UNINTERESTING,
                }
            }
        } else {
            self.tips.push(n);
        }
        self.enqueue_once(n);
        Ok(())
    }

    /// `--ancestry-path=<commit>`: show only commits that are `id`, its
    /// ancestors or its descendants (or those of another target).
    pub fn ancestry_target(&mut self, id: &ObjectId) -> Result<(), S::Error> {
        let n = self.node(id)?;
        self.nodes[n as usize].flags |= TARGET;
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

    /// The commit was excluded (`^id` or reachable from an excluded tip).
    pub fn is_uninteresting(&self, id: &ObjectId) -> bool {
        self.index
            .get(id)
            .is_some_and(|&n| self.nodes[n as usize].flags & UNINTERESTING != 0)
    }

    /// The next commit to show, or `None` when the walk is done.
    pub fn next_commit(&mut self) -> Result<Option<ObjectId>, S::Error> {
        Ok(self.next_shown()?.map(|s| s.id))
    }

    /// The next commit to show with its marks, or `None` when the walk is
    /// done.
    pub fn next_shown(&mut self) -> Result<Option<Shown>, S::Error> {
        if !self.started && self.opts.reverse && self.opts.boundary {
            // `--reverse` turns around the boundary as well.
            self.opts.reverse = false;
            let mut all = VecDeque::new();
            while let Some(s) = self.next_shown()? {
                all.push_front(s);
            }
            self.opts.reverse = true;
            self.reversed = Some(all);
        }
        if let Some(all) = &mut self.reversed {
            return Ok(all.pop_front());
        }
        if !self.started {
            self.start()?;
        }
        let main = match &mut self.prepared {
            Some(list) => list.pop_front(),
            None => self.next_streamed()?,
        };
        if let Some(n) = main {
            self.nodes[n as usize].flags |= SHOWN;
            self.shown_list.push(n);
            return Ok(Some(self.shown_entry(n, false)));
        }
        if !self.opts.boundary {
            return Ok(None);
        }
        if self.boundary_list.is_none() {
            let list = self.collect_boundary()?;
            self.boundary_list = Some(list);
        }
        let next = self.boundary_list.as_mut().and_then(VecDeque::pop_front);
        Ok(next.map(|n| self.shown_entry(n, true)))
    }

    fn shown_entry(&self, n: u32, boundary: bool) -> Shown {
        let node = &self.nodes[n as usize];
        let side = if node.flags & LEFT != 0 {
            Some(Side::Left)
        } else if node.flags & RIGHT != 0 {
            Some(Side::Right)
        } else {
            None
        };
        Shown {
            id: node.id,
            boundary,
            side,
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

    /// Children of `id` met during the walk, the last one first
    /// ([`WalkOptions::children`]).
    pub fn children(&self, id: &ObjectId) -> &[ObjectId] {
        self.index
            .get(id)
            .and_then(|n| self.children.get(n))
            .map_or(&[], Vec::as_slice)
    }

    /// Excluded parents of the selected commits, in the order they are
    /// first met (`--objects-edge`). With exclusions these are the parents
    /// of every commit the walk collected (also those cut off by `-n` or
    /// `--skip`). Call after the walk.
    pub fn edges(&self) -> Vec<ObjectId> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let list = if self.limited {
            &self.limited_list
        } else {
            &self.shown_list
        };
        for &n in list {
            for p in &self.nodes[n as usize].parents {
                if let Some(&pn) = self.index.get(p)
                    && self.nodes[pn as usize].flags & UNINTERESTING != 0
                    && seen.insert(pn)
                {
                    out.push(*p);
                }
            }
        }
        out
    }

    fn start(&mut self) -> Result<(), S::Error> {
        self.started = true;
        if self.opts.no_walk.is_some() && !self.limited {
            return self.prepare_no_walk();
        }
        if self.opts.ancestry_path {
            self.limited = true;
        }
        if self.limited || self.opts.order != Order::Default {
            self.prepare()?;
        } else if self.opts.reverse {
            // Lazy walk, then reverse what it selected.
            let mut list = VecDeque::new();
            while let Some(n) = self.next_streamed()? {
                list.push_front(n);
            }
            self.prepared = Some(list);
        }
        Ok(())
    }

    fn next_streamed(&mut self) -> Result<Option<u32>, S::Error> {
        loop {
            if self.opts.max_count.is_some_and(|m| self.shown >= m) {
                return Ok(None);
            }
            let Some(entry) = self.queue.pop() else {
                return Ok(None);
            };
            self.expand(entry.node)?;
            if !self.passes(entry.node)? {
                continue;
            }
            if self.skipped < self.opts.skip {
                self.skipped += 1;
                continue;
            }
            self.shown += 1;
            return Ok(Some(entry.node));
        }
    }

    fn node(&mut self, id: &ObjectId) -> Result<u32, S::Error> {
        if let Some(&n) = self.index.get(id) {
            return Ok(n);
        }
        let info = self.source.load(id)?;
        let n = u32::try_from(self.nodes.len()).expect("fewer than 2^32 commits");
        let flags = self.pending_flags.remove(id).unwrap_or(0);
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

    fn uninteresting(&self, n: u32) -> bool {
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

    /// Too old for `--since`.
    fn too_old(&self, n: u32) -> bool {
        self.opts
            .max_age
            .is_some_and(|age| self.nodes[n as usize].time < age)
    }

    /// Take `n` out of the queue: put its parents in.
    fn expand(&mut self, n: u32) -> Result<(), S::Error> {
        let uninteresting = self.uninteresting(n);
        if !uninteresting && self.too_old(n) {
            return Ok(());
        }
        let count = if self.opts.first_parent && !uninteresting {
            self.nodes[n as usize].parents.len().min(1)
        } else {
            self.nodes[n as usize].parents.len()
        };
        let sides = self.nodes[n as usize].flags & (LEFT | RIGHT);
        let id = self.nodes[n as usize].id;
        for i in 0..count {
            let p = self.nodes[n as usize].parents[i];
            let pn = self.node(&p)?;
            self.nodes[pn as usize].flags |= sides;
            if self.opts.children && !uninteresting {
                self.children.entry(pn).or_default().insert(0, id);
            }
            if uninteresting {
                self.mark_uninteresting(pn);
            }
            self.enqueue_once(pn);
        }
        Ok(())
    }

    /// The per-commit filters: age, parent count, side and the source's
    /// own.
    fn passes(&mut self, n: u32) -> Result<bool, S::Error> {
        let node = &self.nodes[n as usize];
        if self.too_old(n)
            || self.opts.max_age_filter.is_some_and(|age| node.time < age)
            || self.opts.min_age.is_some_and(|age| node.time > age)
        {
            return Ok(false);
        }
        let parents = node.parents.len();
        if self.opts.min_parents.is_some_and(|m| parents < m)
            || self.opts.max_parents.is_some_and(|m| parents > m)
        {
            return Ok(false);
        }
        let left = node.flags & LEFT != 0;
        if (self.opts.left_only && !left) || (self.opts.right_only && left) {
            return Ok(false);
        }
        let id = node.id;
        self.source.include(&id)
    }

    fn queue_all_uninteresting(&self) -> bool {
        self.queue.iter().all(|e| self.uninteresting(e.node))
    }

    /// Collect the whole result, then sort, filter, skip, cut and reverse
    /// it.
    fn prepare(&mut self) -> Result<(), S::Error> {
        let mut list = self.limit()?;
        if self.opts.ancestry_path {
            list = self.ancestry_filter(list);
        }
        self.limited_list.clone_from(&list);
        match self.opts.order {
            Order::Default => {}
            Order::Date | Order::Topo => {
                list = self.sort_topologically(&list, self.opts.order, None)
            }
            Order::AuthorDate => {
                let mut times = HashMap::new();
                for &n in &list {
                    let id = self.nodes[n as usize].id;
                    let t = self.source.author_time(&id)?;
                    times.insert(n, t.unwrap_or(self.nodes[n as usize].time));
                }
                list = self.sort_topologically(&list, Order::AuthorDate, Some(&times));
            }
        }
        self.finish(list)
    }

    /// Filter, skip, cut and reverse `list`, which becomes the output.
    fn finish(&mut self, list: Vec<u32>) -> Result<(), S::Error> {
        let mut out = VecDeque::new();
        let mut skip = self.opts.skip;
        let max = self.opts.max_count.unwrap_or(usize::MAX);
        for n in list {
            if out.len() >= max {
                break;
            }
            if !self.passes(n)? {
                continue;
            }
            if skip > 0 {
                skip -= 1;
                continue;
            }
            out.push_back(n);
        }
        if self.opts.reverse {
            out.make_contiguous().reverse();
        }
        self.prepared = Some(out);
        Ok(())
    }

    /// `--no-walk`: the interesting tips themselves.
    fn prepare_no_walk(&mut self) -> Result<(), S::Error> {
        let mut seen = HashSet::new();
        let mut list: Vec<u32> = self
            .tips
            .iter()
            .copied()
            .filter(|n| seen.insert(*n))
            .collect();
        if self.opts.no_walk == Some(NoWalk::Sorted) {
            // Stable: equal dates keep the order of the command line.
            list.sort_by(|a, b| {
                self.nodes[*b as usize]
                    .time
                    .cmp(&self.nodes[*a as usize].time)
            });
        }
        self.finish(list)
    }

    /// Walk until no excluded commit can reach a collected one any more.
    ///
    /// The stopping rule follows the observable behaviour of the reference
    /// implementation: once only excluded commits are queued, every further
    /// excluded commit taken out counts down [`SLOP`], unless the newest
    /// queued commit is at least as new as the last collected one, which
    /// starts the count again.
    fn limit(&mut self) -> Result<Vec<u32>, S::Error> {
        let mut list = Vec::new();
        let mut last_collected: Option<i64> = None;
        let mut slop = SLOP;
        while let Some(entry) = self.queue.pop() {
            let n = entry.node;
            if !self.uninteresting(n) && self.too_old(n) {
                // Too old for `--since`: neither shown nor walked further.
                continue;
            }
            self.expand(n)?;
            if !self.uninteresting(n) {
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
        list.retain(|&n| !self.uninteresting(n));
        Ok(list)
    }

    /// `--ancestry-path`: keep the commits of `list` that are a target, an
    /// ancestor of one or a descendant of one. Without explicit targets
    /// the excluded tips are the targets.
    fn ancestry_filter(&mut self, list: Vec<u32>) -> Vec<u32> {
        let explicit = self.nodes.iter().any(|n| n.flags & TARGET != 0);
        let target_flag = if explicit { TARGET } else { BOTTOM };
        let walked = |w: &Self, n: u32| -> Vec<u32> {
            let node = &w.nodes[n as usize];
            let count = if w.opts.first_parent && node.flags & UNINTERESTING == 0 {
                node.parents.len().min(1)
            } else {
                node.parents.len()
            };
            node.parents[..count]
                .iter()
                .filter_map(|p| w.index.get(p).copied())
                .collect()
        };
        // Descendants: a commit with a parent that is a target or related.
        let mut changed = true;
        while changed {
            changed = false;
            for &n in list.iter().rev() {
                if self.nodes[n as usize].flags & RELATED != 0 {
                    continue;
                }
                let hit = walked(self, n)
                    .into_iter()
                    .any(|p| self.nodes[p as usize].flags & (RELATED | target_flag) != 0);
                if hit {
                    self.nodes[n as usize].flags |= RELATED;
                    changed = true;
                }
            }
        }
        // Ancestors of the targets.
        let mut stack: Vec<u32> = (0..self.nodes.len() as u32)
            .filter(|&n| self.nodes[n as usize].flags & target_flag != 0)
            .collect();
        let mut seen: HashSet<u32> = stack.iter().copied().collect();
        while let Some(n) = stack.pop() {
            self.nodes[n as usize].flags |= RELATED;
            for p in walked(self, n) {
                if seen.insert(p) {
                    stack.push(p);
                }
            }
        }
        list.into_iter()
            .filter(|&n| self.nodes[n as usize].flags & RELATED != 0)
            .collect()
    }

    /// The boundary: parents of shown commits that were not shown, last
    /// met first.
    fn collect_boundary(&mut self) -> Result<VecDeque<u32>, S::Error> {
        let mut out = VecDeque::new();
        for i in 0..self.shown_list.len() {
            let n = self.shown_list[i];
            // All parents, also with `--first-parent`.
            for j in 0..self.nodes[n as usize].parents.len() {
                let p = self.nodes[n as usize].parents[j];
                let known = self.index.contains_key(&p);
                let pn = self.node(&p)?;
                if !known {
                    self.nodes[pn as usize].flags |= UNREACHED;
                }
                let flags = &mut self.nodes[pn as usize].flags;
                if *flags & (SHOWN | BOUNDARY) != 0 {
                    continue;
                }
                *flags |= BOUNDARY;
                out.push_front(pn);
            }
        }
        out.retain(|&n| self.nodes[n as usize].flags & SHOWN == 0);
        // The boundary is sorted like the output, so that no parent comes
        // before its child; `--date-order` picks by date, everything else
        // keeps lines of history together.
        let order = match self.opts.order {
            Order::Date => Order::Date,
            _ => Order::Topo,
        };
        let list: Vec<u32> = out.into_iter().collect();
        Ok(self.sort_topologically(&list, order, None).into())
    }

    fn sort_topologically(
        &self,
        list: &[u32],
        order: Order,
        author_times: Option<&HashMap<u32, i64>>,
    ) -> Vec<u32> {
        // Children still to be shown, per commit; -1 = not in the result.
        let mut indegree = vec![-1i64; self.nodes.len()];
        for &n in list {
            indegree[n as usize] = 0;
        }
        // All parents count, also with `--first-parent`: a commit reached
        // only through first parents still waits for children that merged
        // it as a later parent. A boundary commit the walk never reached
        // does not order its parents.
        let parents_of = |n: u32| {
            let node = &self.nodes[n as usize];
            let parents: &[ObjectId] = if node.flags & UNREACHED == 0 {
                &node.parents
            } else {
                &[]
            };
            parents.iter().filter_map(|p| self.index.get(p).copied())
        };
        for &n in list {
            for p in parents_of(n) {
                if indegree[p as usize] >= 0 {
                    indegree[p as usize] += 1;
                }
            }
        }
        let mut out = Vec::with_capacity(list.len());
        match order {
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
            Order::Date | Order::AuthorDate | Order::Default => {
                let time = |n: u32| match author_times {
                    Some(t) => t.get(&n).copied().unwrap_or(self.nodes[n as usize].time),
                    None => self.nodes[n as usize].time,
                };
                let mut seq = 0u64;
                let mut heap = BinaryHeap::new();
                let mut push = |heap: &mut BinaryHeap<Entry>, n: u32| {
                    seq += 1;
                    heap.push(Entry {
                        time: time(n),
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
