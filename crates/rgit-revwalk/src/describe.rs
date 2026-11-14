// SPDX-License-Identifier: MIT
//! Finding the nearest named ancestor of a commit (`git describe`).
//!
//! Written from `Documentation/git-describe.adoc` ("SEARCH STRATEGY") and
//! the observable behaviour of the `git` binary (`--debug` output on
//! git.git and hand-made histories). The candidate bookkeeping (one bit per
//! candidate, depth counted while walking, a final walk that completes the
//! depth of the best candidate) follows gitoxide's
//! `gix-revision/src/describe.rs` (MIT OR Apache-2.0, see
//! THIRD-PARTY-NOTICES.md), adjusted to the stopping rules git applies.
//!
//! # The walk
//!
//! Commits come out of a queue ordered by committer date, newest first;
//! equal dates come out in the order they were queued. Every commit carries
//! the set of candidates it is reachable from. A named commit that is taken
//! out becomes a candidate; its *depth* starts at the number of commits
//! taken out before it, and every later commit that is not reachable from
//! it adds one. The depth of a candidate is therefore the number of commits
//! in `<candidate>..<commit>`, once the walk has gone far enough.
//!
//! The walk stops
//! - when a commit is taken out while the candidate list is full (the
//!   maximum, or as many candidates as there are names): "gave up";
//! - when, after a commit, the queue is empty, an annotated candidate
//!   exists and the commit is reachable from all candidates of the
//!   smallest depth so far: "finished".
//!
//! Candidates are then ordered by depth and discovery order. Only the
//! first one's depth is completed by walking on until every queued commit
//! is reachable from it; the others keep the depth found so far.

use std::collections::{BinaryHeap, HashMap};

use rgit_hash::ObjectId;

use crate::walk::{CommitInfo, CommitSource};

/// Most candidates the walk tracks at once (larger requests are capped).
pub const MAX_CANDIDATES: usize = 28;

/// What a name on a commit means for the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameUse {
    /// May describe the commit; `annotated` names allow the early stop.
    Usable { annotated: bool },
    /// Known, but not allowed (a lightweight tag without `--tags`). Only
    /// counted, for the "try --tags" hint.
    Unusable,
}

/// Options of [`describe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// `--candidates` (capped at [`MAX_CANDIDATES`]).
    pub max_candidates: usize,
    /// `--first-parent`.
    pub first_parent: bool,
    /// Number of names known in total; the walk gives up once that many
    /// candidates were found.
    pub total_names: usize,
}

/// A candidate found by the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The named commit.
    pub commit: ObjectId,
    /// Commits in `<candidate>..<target>` (final only for the first
    /// candidate of [`Outcome::candidates`]).
    pub depth: usize,
    /// Whether the name is annotated.
    pub annotated: bool,
}

/// Result of [`describe`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Candidates, best first.
    pub candidates: Vec<Candidate>,
    /// Commits taken out of the queue, including the final depth walk.
    pub traversed: usize,
    /// The commit the walk gave up at (candidate list full).
    pub gave_up_at: Option<ObjectId>,
    /// The commit after which the walk finished early.
    pub finished_at: Option<ObjectId>,
    /// Named commits passed over because their name was not usable.
    pub unusable_seen: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    time: i64,
    seq: u64,
    node: u32,
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
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

struct Node {
    id: ObjectId,
    info: Option<CommitInfo>,
    time: i64,
    within: u32,
    seen: bool,
}

struct Graph<'s, S: CommitSource> {
    source: &'s mut S,
    nodes: Vec<Node>,
    index: HashMap<ObjectId, u32>,
    queue: BinaryHeap<Entry>,
    seq: u64,
    first_parent: bool,
}

impl<S: CommitSource> Graph<'_, S> {
    fn node(&mut self, id: &ObjectId) -> Result<u32, S::Error> {
        if let Some(&n) = self.index.get(id) {
            return Ok(n);
        }
        let info = self.source.load(id)?;
        let n = u32::try_from(self.nodes.len()).expect("fewer than 2^32 commits");
        self.nodes.push(Node {
            id: *id,
            time: info.time,
            info: Some(info),
            within: 0,
            seen: false,
        });
        self.index.insert(*id, n);
        Ok(n)
    }

    fn enqueue(&mut self, n: u32) {
        self.seq += 1;
        self.queue.push(Entry {
            time: self.nodes[n as usize].time,
            seq: self.seq,
            node: n,
        });
    }

    fn pop(&mut self) -> Option<u32> {
        self.queue.pop().map(|e| e.node)
    }

    /// Queue the parents of `n` not seen yet; all of them inherit the
    /// candidate bits of `n`.
    fn push_parents(&mut self, n: u32) -> Result<(), S::Error> {
        let within = self.nodes[n as usize].within;
        let mut parents = self.nodes[n as usize]
            .info
            .as_ref()
            .map(|i| i.parents.clone())
            .unwrap_or_default();
        if self.first_parent {
            parents.truncate(1);
        }
        for p in &parents {
            let pn = self.node(p)?;
            let node = &mut self.nodes[pn as usize];
            if !node.seen {
                node.seen = true;
                self.enqueue(pn);
            }
            self.nodes[pn as usize].within |= within;
        }
        Ok(())
    }
}

/// Describe `target` (a commit without an exact name): walk its history
/// and collect candidates from `name_of`.
pub fn describe<S: CommitSource>(
    source: &mut S,
    target: &ObjectId,
    name_of: impl Fn(&ObjectId) -> Option<NameUse>,
    opts: Options,
) -> Result<Outcome, S::Error> {
    let max = opts.max_candidates.min(MAX_CANDIDATES);
    let mut g = Graph {
        source,
        nodes: Vec::new(),
        index: HashMap::new(),
        queue: BinaryHeap::new(),
        seq: 0,
        first_parent: opts.first_parent,
    };
    let mut out = Outcome::default();
    // (commit node, bit, depth, discovery order, annotated)
    let mut found: Vec<(u32, u32, usize, usize, bool)> = Vec::new();
    let mut annotated = 0usize;

    let start = g.node(target)?;
    g.nodes[start as usize].seen = true;
    g.enqueue(start);
    while let Some(n) = g.pop() {
        out.traversed += 1;
        if found.len() == max || found.len() == opts.total_names {
            out.gave_up_at = Some(g.nodes[n as usize].id);
            // Taken out again by the depth walk.
            out.traversed -= 1;
            g.enqueue(n);
            break;
        }
        let id = g.nodes[n as usize].id;
        match name_of(&id) {
            Some(NameUse::Usable { annotated: a }) => {
                let bit = 1u32 << found.len();
                g.nodes[n as usize].within |= bit;
                found.push((n, bit, out.traversed - 1, found.len(), a));
                if a {
                    annotated += 1;
                }
            }
            Some(NameUse::Unusable) => out.unusable_seen += 1,
            None => {}
        }
        let within = g.nodes[n as usize].within;
        for c in found.iter_mut() {
            if within & c.1 == 0 {
                c.2 += 1;
            }
        }
        if annotated > 0 && g.queue.is_empty() {
            // A single line of history left: stop once this commit is
            // reachable from every candidate of the smallest depth.
            let min = found.iter().map(|c| c.2).min().unwrap_or(0);
            let best = found
                .iter()
                .filter(|c| c.2 == min)
                .fold(0u32, |acc, c| acc | c.1);
            if within & best == best {
                out.finished_at = Some(id);
                break;
            }
        }
        g.push_parents(n)?;
    }
    if found.is_empty() {
        return Ok(out);
    }
    found.sort_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)));

    // Complete the depth of the best candidate.
    let best_bit = found[0].1;
    while let Some(n) = g.pop() {
        out.traversed += 1;
        if g.nodes[n as usize].within & best_bit != 0 {
            let all = g
                .queue
                .iter()
                .all(|e| g.nodes[e.node as usize].within & best_bit != 0);
            if all {
                break;
            }
        } else {
            found[0].2 += 1;
        }
        g.push_parents(n)?;
    }

    out.candidates = found
        .into_iter()
        .map(|(n, _, depth, _, annotated)| Candidate {
            commit: g.nodes[n as usize].id,
            depth,
            annotated,
        })
        .collect();
    Ok(out)
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

    struct Fake(HashMap<ObjectId, CommitInfo>);

    impl CommitSource for Fake {
        type Error = ObjectId;
        fn load(&mut self, i: &ObjectId) -> Result<CommitInfo, ObjectId> {
            self.0.get(i).cloned().ok_or(*i)
        }
    }

    /// z(0) - a1(10, A annotated) - a2(20) ------ M(30)
    ///    \-- b1(15, L lightweight) - b2(25) --/
    fn graph() -> Fake {
        let mut m = HashMap::new();
        let mut add = |n: u8, time: i64, parents: &[u8]| {
            m.insert(
                id(n),
                CommitInfo {
                    parents: parents.iter().map(|&p| id(p)).collect(),
                    time,
                },
            );
        };
        add(1, 0, &[]);
        add(2, 10, &[1]);
        add(3, 20, &[2]);
        add(4, 15, &[1]);
        add(5, 25, &[4]);
        add(6, 30, &[3, 5]);
        Fake(m)
    }

    fn names(tags: bool) -> impl Fn(&ObjectId) -> Option<NameUse> {
        move |i: &ObjectId| {
            if *i == id(2) {
                Some(NameUse::Usable { annotated: true })
            } else if *i == id(4) {
                Some(if tags {
                    NameUse::Usable { annotated: false }
                } else {
                    NameUse::Unusable
                })
            } else {
                None
            }
        }
    }

    fn opts(max: usize, first_parent: bool) -> Options {
        Options {
            max_candidates: max,
            first_parent,
            total_names: 2,
        }
    }

    #[test]
    fn annotated_only_finishes_when_the_queue_runs_dry() {
        let o = describe(&mut graph(), &id(6), names(false), opts(10, false)).unwrap();
        assert_eq!(o.candidates.len(), 1);
        assert_eq!((o.candidates[0].commit, o.candidates[0].depth), (id(2), 4));
        assert_eq!(o.finished_at, Some(id(1)));
        assert_eq!(o.traversed, 6);
        assert_eq!(o.unusable_seen, 1);
    }

    #[test]
    fn gives_up_once_every_name_is_a_candidate() {
        let o = describe(&mut graph(), &id(6), names(true), opts(10, false)).unwrap();
        let got: Vec<_> = o.candidates.iter().map(|c| (c.commit, c.depth)).collect();
        // Equal depth: the first found wins.
        assert_eq!(got, vec![(id(4), 4), (id(2), 4)]);
        assert_eq!(o.gave_up_at, Some(id(1)));
        assert_eq!(o.traversed, 6);
    }

    #[test]
    fn full_candidate_list_gives_up_and_completes_the_depth() {
        let o = describe(&mut graph(), &id(6), names(true), opts(1, false)).unwrap();
        assert_eq!(o.candidates.len(), 1);
        assert_eq!((o.candidates[0].commit, o.candidates[0].depth), (id(4), 4));
        assert_eq!(o.gave_up_at, Some(id(2)));
        assert_eq!(o.traversed, 6);
    }

    #[test]
    fn first_parent_follows_one_line() {
        let o = describe(&mut graph(), &id(6), names(true), opts(10, true)).unwrap();
        assert_eq!((o.candidates[0].commit, o.candidates[0].depth), (id(2), 2));
        assert_eq!(o.finished_at, Some(id(2)));
        assert_eq!(o.traversed, 3);
    }
}
