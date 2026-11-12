// SPDX-License-Identifier: MIT
//! Choosing the `have` lines of a fetch.
//!
//! The client walks its own history and offers commits to the server until
//! the server knows enough common commits to build a small pack
//! (`Documentation/gitprotocol-pack.adoc`, "Packfile Negotiation";
//! `fetch.negotiationAlgorithm` in `Documentation/config/fetch.adoc`).
//!
//! [`ConsecutiveNegotiator`] is the default (`consecutive`) algorithm: it
//! visits the local commits newest first ("date order", ties in the order
//! the commits were queued) and offers each one that is not yet known to be
//! common. Once a commit is common (ACKed by the server), its ancestors are
//! common as well and are not offered. A local commit the server advertised
//! as a ref tip is offered in date order like the others, but its
//! ancestors are treated as common right away (as observed from git
//! 2.56.0). The walk is over when only common commits are left.
//!
//! The commit graph comes from a [`CommitSource`], so this module does not
//! depend on the object database.

use std::collections::{BinaryHeap, HashMap};

use rgit_hash::ObjectId;

/// What the client offers during negotiation.
pub trait Negotiator {
    /// The next commit to send as `have`, or `None` when there is nothing
    /// left to offer.
    fn next_have(&mut self) -> Option<ObjectId>;

    /// The server ACKed `id`. Returns `true` if `id` was not known to be
    /// common before.
    fn ack(&mut self, id: &ObjectId) -> bool;
}

/// A negotiator that offers nothing (`fetch.negotiationAlgorithm=noop`, and
/// the clone case).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNegotiator;

impl Negotiator for NoopNegotiator {
    fn next_have(&mut self) -> Option<ObjectId> {
        None
    }

    fn ack(&mut self, _id: &ObjectId) -> bool {
        false
    }
}

/// The parts of a commit the negotiator needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Committer time (seconds since the epoch).
    pub time: i64,
    /// Parent ids.
    pub parents: Vec<ObjectId>,
}

/// Access to the local commit graph.
pub trait CommitSource {
    /// The commit `id`, or `None` if it is missing locally or is not a
    /// commit; such ids are skipped.
    fn commit(&mut self, id: &ObjectId) -> Option<CommitInfo>;
}

impl<F: FnMut(&ObjectId) -> Option<CommitInfo>> CommitSource for F {
    fn commit(&mut self, id: &ObjectId) -> Option<CommitInfo> {
        self(id)
    }
}

const SEEN: u8 = 1;
const COMMON: u8 = 2;
const POPPED: u8 = 4;
const KNOWN: u8 = 8;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    time: i64,
    // Lower sequence numbers first among equal times.
    seq: std::cmp::Reverse<u64>,
    id: ObjectId,
}

/// The `consecutive` negotiation algorithm.
#[derive(Debug)]
pub struct ConsecutiveNegotiator<S> {
    source: S,
    flags: HashMap<ObjectId, u8>,
    /// Parents of commits already taken from the queue.
    parents: HashMap<ObjectId, Vec<ObjectId>>,
    queue: BinaryHeap<Entry>,
    seq: u64,
    /// Queued entries not marked common.
    non_common: usize,
}

impl<S: CommitSource> ConsecutiveNegotiator<S> {
    /// A negotiator reading commits from `source`.
    pub fn new(source: S) -> ConsecutiveNegotiator<S> {
        ConsecutiveNegotiator {
            source,
            flags: HashMap::new(),
            parents: HashMap::new(),
            queue: BinaryHeap::new(),
            seq: 0,
            non_common: 0,
        }
    }

    /// Start the walk at a local tip (a ref of the local repository).
    pub fn add_tip(&mut self, id: ObjectId) {
        self.push(id, 0);
    }

    /// A local commit that the server advertised as one of its ref tips.
    /// It is queued like a tip and offered in date order, but its ancestors
    /// are common without asking. git calls this before adding the tips, so
    /// among commits with equal times these come first.
    pub fn known_common(&mut self, id: ObjectId) {
        self.push(id, KNOWN);
    }

    fn flag(&self, id: &ObjectId) -> u8 {
        self.flags.get(id).copied().unwrap_or(0)
    }

    /// Queue `id` with `mark` if it was not seen yet; returns whether it was
    /// queued.
    fn push(&mut self, id: ObjectId, mark: u8) -> bool {
        let flag = self.flags.entry(id).or_insert(0);
        if *flag & SEEN != 0 {
            return false;
        }
        let Some(info) = self.source.commit(&id) else {
            *flag |= SEEN | POPPED | mark;
            return false;
        };
        *flag |= SEEN | mark;
        if mark & COMMON == 0 {
            self.non_common += 1;
        }
        self.seq += 1;
        self.queue.push(Entry {
            time: info.time,
            seq: std::cmp::Reverse(self.seq),
            id,
        });
        self.parents.insert(id, info.parents);
        true
    }

    /// Mark `id` and its ancestors common. Ancestors still in the queue are
    /// marked when they are taken from it.
    fn mark_common(&mut self, id: ObjectId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let flag = self.flag(&id);
            if flag & COMMON != 0 {
                continue;
            }
            if flag & SEEN == 0 {
                // Not reached by the walk: remember, but do not queue.
                self.flags.insert(id, SEEN | POPPED | COMMON);
                continue;
            }
            self.flags.insert(id, flag | COMMON);
            if flag & POPPED == 0 {
                self.non_common -= 1;
            } else if let Some(parents) = self.parents.get(&id) {
                stack.extend(parents.iter().copied());
            }
        }
    }
}

impl<S: CommitSource> Negotiator for ConsecutiveNegotiator<S> {
    fn next_have(&mut self) -> Option<ObjectId> {
        loop {
            if self.non_common == 0 {
                return None;
            }
            let entry = self.queue.pop()?;
            let id = entry.id;
            let flag = self.flag(&id);
            self.flags.insert(id, flag | POPPED);
            let common = flag & COMMON != 0;
            if !common {
                self.non_common -= 1;
            }
            let parents_common = common || flag & KNOWN != 0;
            let parents = self.parents.get(&id).cloned().unwrap_or_default();
            for p in parents {
                if parents_common {
                    if self.flag(&p) & SEEN != 0 {
                        self.mark_common(p);
                    } else {
                        self.push(p, COMMON);
                    }
                } else {
                    self.push(p, 0);
                }
            }
            if !common {
                return Some(id);
            }
        }
    }

    fn ack(&mut self, id: &ObjectId) -> bool {
        if self.flag(id) & COMMON != 0 {
            return false;
        }
        self.mark_common(*id);
        true
    }
}

/// Number of `have` lines in the first request.
pub const INITIAL_HAVES: usize = 16;

/// Number of `have` lines in the request after one with `count` lines:
/// doubled up to 16384, then grown by a tenth (observed from git 2.56.0).
pub fn next_have_count(count: usize) -> usize {
    if count < 16384 {
        count * 2
    } else {
        count + count / 10
    }
}

/// After the first ACK, the client gives up and sends `done` once this many
/// `have` lines went out without a new ACK.
pub const MAX_IN_VAIN: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;
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

    fn drain(n: &mut impl Negotiator) -> Vec<ObjectId> {
        std::iter::from_fn(|| n.next_have()).collect()
    }

    #[test]
    fn linear_date_order() {
        let mut n = ConsecutiveNegotiator::new(graph(&[(1, &[]), (2, &[1]), (3, &[2])]));
        n.add_tip(id(3));
        assert_eq!(drain(&mut n), vec![id(3), id(2), id(1)]);
    }

    #[test]
    fn merge_walks_newest_first() {
        // 1 - 2 - 4 - 6
        //  \- 3 - 5 -/
        let mut n = ConsecutiveNegotiator::new(graph(&[
            (1, &[]),
            (2, &[1]),
            (3, &[1]),
            (4, &[2]),
            (5, &[3]),
            (6, &[4, 5]),
        ]));
        n.add_tip(id(6));
        assert_eq!(
            drain(&mut n),
            vec![id(6), id(5), id(4), id(3), id(2), id(1)]
        );
    }

    #[test]
    fn ack_stops_ancestors() {
        let mut n = ConsecutiveNegotiator::new(graph(&[
            (1, &[]),
            (2, &[1]),
            (3, &[2]),
            (4, &[3]),
            (10, &[]),
        ]));
        n.add_tip(id(4));
        n.add_tip(id(10));
        assert_eq!(n.next_have(), Some(id(10)));
        assert_eq!(n.next_have(), Some(id(4)));
        assert_eq!(n.next_have(), Some(id(3)));
        assert!(n.ack(&id(3)));
        assert!(!n.ack(&id(3)));
        // 2 is queued and now common; nothing else is left.
        assert_eq!(n.next_have(), None);
    }

    #[test]
    fn ack_of_queued_commit() {
        let mut n = ConsecutiveNegotiator::new(graph(&[(1, &[]), (2, &[1]), (3, &[2]), (5, &[])]));
        n.add_tip(id(3));
        n.add_tip(id(5));
        assert_eq!(n.next_have(), Some(id(5)));
        // 3 is still queued.
        assert!(n.ack(&id(3)));
        assert_eq!(n.next_have(), None);
    }

    #[test]
    fn known_common_offered_in_date_order_ancestors_skipped() {
        let mut n = ConsecutiveNegotiator::new(graph(&[
            (1, &[]),
            (2, &[1]),
            (3, &[2]),
            (4, &[3]),
            (5, &[]),
        ]));
        n.known_common(id(2));
        n.add_tip(id(4));
        n.add_tip(id(5));
        assert_eq!(drain(&mut n), vec![id(5), id(4), id(3), id(2)]);
    }

    #[test]
    fn missing_commits_are_skipped() {
        let mut n = ConsecutiveNegotiator::new(graph(&[(2, &[1]), (3, &[2])]));
        n.add_tip(id(3));
        n.add_tip(id(9));
        assert_eq!(drain(&mut n), vec![id(3), id(2)]);
    }

    #[test]
    fn equal_times_keep_insertion_order() {
        let g = move |i: &ObjectId| {
            Some(CommitInfo {
                time: 0,
                parents: if *i == id(1) { vec![] } else { vec![id(1)] },
            })
        };
        let mut n = ConsecutiveNegotiator::new(g);
        n.add_tip(id(7));
        n.add_tip(id(5));
        n.add_tip(id(6));
        assert_eq!(drain(&mut n), vec![id(7), id(5), id(6), id(1)]);
    }

    #[test]
    fn have_counts() {
        let mut c = INITIAL_HAVES;
        let mut seen = vec![c];
        for _ in 0..11 {
            c = next_have_count(c);
            seen.push(c);
        }
        assert_eq!(
            seen,
            [
                16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 18022
            ]
        );
    }
}
