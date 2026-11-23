// SPDX-License-Identifier: MIT
//! The `skipping` negotiation algorithm (`fetch.negotiationAlgorithm`,
//! `Documentation/config/fetch.adoc`): like `consecutive` it walks the
//! local history newest first, but after each `have` it skips a growing
//! number of commits, so it reaches the history the server knows sooner at
//! the cost of a possibly larger pack.
//!
//! As observed from git 2.56.0 (and described by t5552):
//!
//! - every queued commit carries a skip distance and the number of commits
//!   still to skip; tips start at 0/0;
//! - a commit with nothing left to skip is sent; its parents get the next
//!   distance (1 after 0, then `d * 3 / 2 + 1`: 1, 2, 4, 7, 11, 17, ...)
//!   with that many commits to skip;
//! - a skipped commit hands its distance on to its parents with one commit
//!   less to skip;
//! - a commit reached twice keeps the larger distance;
//! - a commit none of whose parents can still be queued (a root, or one
//!   whose parents were already taken from the queue) is sent even when it
//!   should be skipped;
//! - ACKed commits and the ancestors of advertised ones are common and
//!   never sent.

use std::collections::{BinaryHeap, HashMap};

use rgit_hash::ObjectId;

use super::negotiate::{CommitSource, Negotiator};

#[derive(Debug, Default, Clone)]
struct State {
    seen: bool,
    popped: bool,
    common: bool,
    /// Advertised by the server: sent, but its ancestors are common.
    known: bool,
    distance: u32,
    to_skip: u32,
    parents: Vec<ObjectId>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    time: i64,
    seq: std::cmp::Reverse<u64>,
    id: ObjectId,
}

/// The `skipping` negotiation algorithm.
#[derive(Debug)]
pub struct SkippingNegotiator<S> {
    source: S,
    states: HashMap<ObjectId, State>,
    queue: BinaryHeap<Entry>,
    seq: u64,
    /// Queued entries not marked common.
    non_common: usize,
}

fn next_distance(d: u32) -> u32 {
    if d == 0 { 1 } else { d * 3 / 2 + 1 }
}

impl<S: CommitSource> SkippingNegotiator<S> {
    /// A negotiator reading commits from `source`.
    pub fn new(source: S) -> SkippingNegotiator<S> {
        SkippingNegotiator {
            source,
            states: HashMap::new(),
            queue: BinaryHeap::new(),
            seq: 0,
            non_common: 0,
        }
    }

    /// Start the walk at a local tip.
    pub fn add_tip(&mut self, id: ObjectId) {
        self.push(id, false, 0, 0);
    }

    /// A local commit that the server advertised as one of its ref tips:
    /// offered like the others, its ancestors are common.
    pub fn known_common(&mut self, id: ObjectId) {
        if self.push(id, false, 0, 0) {
            if let Some(s) = self.states.get_mut(&id) {
                s.known = true;
            }
        }
    }

    /// Queue `id` if it was not seen yet; returns whether it was queued.
    fn push(&mut self, id: ObjectId, common: bool, distance: u32, to_skip: u32) -> bool {
        if self.states.get(&id).is_some_and(|s| s.seen) {
            return false;
        }
        let Some(info) = self.source.commit(&id) else {
            self.states.insert(
                id,
                State {
                    seen: true,
                    popped: true,
                    common,
                    ..State::default()
                },
            );
            return false;
        };
        self.states.insert(
            id,
            State {
                seen: true,
                common,
                distance,
                to_skip,
                parents: info.parents,
                ..State::default()
            },
        );
        if !common {
            self.non_common += 1;
        }
        self.seq += 1;
        self.queue.push(Entry {
            time: info.time,
            seq: std::cmp::Reverse(self.seq),
            id,
        });
        true
    }

    /// Mark `id` and its ancestors common.
    fn mark_common(&mut self, id: ObjectId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let s = self.states.entry(id).or_default();
            if s.common {
                continue;
            }
            if !s.seen {
                // Not reached by the walk: remember, but do not queue.
                s.seen = true;
                s.popped = true;
                s.common = true;
                continue;
            }
            s.common = true;
            if !s.popped {
                self.non_common -= 1;
            } else {
                stack.extend(s.parents.iter().copied());
            }
        }
    }
}

impl<S: CommitSource> Negotiator for SkippingNegotiator<S> {
    fn next_have(&mut self) -> Option<ObjectId> {
        loop {
            if self.non_common == 0 {
                return None;
            }
            let entry = self.queue.pop()?;
            let id = entry.id;
            let state = {
                let s = self
                    .states
                    .get_mut(&id)
                    .expect("queued commits have a state");
                s.popped = true;
                s.clone()
            };
            if !state.common {
                self.non_common -= 1;
            }
            if state.common || state.known {
                for p in &state.parents {
                    if self.states.get(p).is_some_and(|s| s.seen) {
                        self.mark_common(*p);
                    } else {
                        self.push(*p, true, 0, 0);
                    }
                }
                if state.common {
                    continue;
                }
                return Some(id);
            }
            let open_parents = state
                .parents
                .iter()
                .filter(|p| !self.states.get(p).is_some_and(|s| s.popped))
                .count();
            let send = state.to_skip == 0 || open_parents == 0;
            let (distance, to_skip) = if send {
                let d = next_distance(state.distance);
                (d, d)
            } else {
                (state.distance, state.to_skip - 1)
            };
            for p in &state.parents {
                match self.states.get_mut(p) {
                    Some(s) if s.seen => {
                        if !s.popped && !s.common && distance > s.distance {
                            s.distance = distance;
                            s.to_skip = to_skip;
                        }
                    }
                    _ => {
                        self.push(*p, false, distance, to_skip);
                    }
                }
            }
            if send {
                return Some(id);
            }
        }
    }

    fn ack(&mut self, id: &ObjectId) -> bool {
        if self.states.get(id).is_some_and(|s| s.common) {
            return false;
        }
        self.mark_common(*id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::negotiate::CommitInfo;
    use rgit_hash::HashKind;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
    }

    /// Commits `n` with time `n` and the given parents.
    fn graph(edges: &[(u8, &[u8])]) -> impl FnMut(&ObjectId) -> Option<CommitInfo> {
        let map: HashMap<ObjectId, CommitInfo> = edges
            .iter()
            .map(|(c, ps)| {
                (
                    id(*c),
                    CommitInfo {
                        time: i64::from(*c),
                        parents: ps.iter().map(|p| id(*p)).collect(),
                    },
                )
            })
            .collect();
        move |i: &ObjectId| map.get(i).cloned()
    }

    fn chain(n: u8) -> Vec<(u8, Vec<u8>)> {
        (1..=n)
            .map(|i| (i, if i == 1 { vec![] } else { vec![i - 1] }))
            .collect()
    }

    fn drain(n: &mut impl Negotiator) -> Vec<ObjectId> {
        std::iter::from_fn(|| n.next_have()).collect()
    }

    #[test]
    fn distances_grow() {
        let edges = chain(60);
        let refs: Vec<(u8, &[u8])> = edges.iter().map(|(c, p)| (*c, p.as_slice())).collect();
        let mut n = SkippingNegotiator::new(graph(&refs));
        // Every commit is also a tip (tags), as in t5552.
        for i in (1..=60).rev() {
            n.add_tip(id(i));
        }
        let sent: Vec<ObjectId> = [60, 58, 55, 50, 42, 30, 12, 1]
            .iter()
            .map(|i| id(*i))
            .collect();
        assert_eq!(drain(&mut n), sent);
    }

    #[test]
    fn larger_distance_wins() {
        // 1..11 linear, 12 ("c5side") on top of 5.
        let mut edges = chain(11);
        edges.push((12, vec![5]));
        let refs: Vec<(u8, &[u8])> = edges.iter().map(|(c, p)| (*c, p.as_slice())).collect();
        let mut n = SkippingNegotiator::new(graph(&refs));
        n.add_tip(id(12));
        n.add_tip(id(11));
        let sent: Vec<ObjectId> = [12, 11, 9, 6, 1].iter().map(|i| id(*i)).collect();
        assert_eq!(drain(&mut n), sent);
    }

    #[test]
    fn ack_stops_ancestors() {
        let edges = chain(20);
        let refs: Vec<(u8, &[u8])> = edges.iter().map(|(c, p)| (*c, p.as_slice())).collect();
        let mut n = SkippingNegotiator::new(graph(&refs));
        n.add_tip(id(20));
        assert_eq!(n.next_have(), Some(id(20)));
        assert_eq!(n.next_have(), Some(id(18)));
        assert!(n.ack(&id(18)));
        assert_eq!(n.next_have(), None);
    }
}
