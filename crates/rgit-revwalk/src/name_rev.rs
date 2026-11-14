// SPDX-License-Identifier: MIT
//! Naming commits relative to references (`git name-rev`, `git describe
//! --contains`).
//!
//! Written from `Documentation/git-name-rev.adoc` and the observable
//! behaviour of the `git` binary (names of every commit of git.git and of
//! hand-made histories).
//!
//! Every reference tip names its commit; names travel down to parents:
//! the first parent of `X` is `X~1` (`X~<n>` adds up), parent `<k>` of a
//! merge is `X^<k>`. A commit keeps the best name offered to it:
//!
//! 1. names from tags beat names from other references;
//! 2. among tag names the older tag (tagger date, commit date for
//!    lightweight tags) wins; for equal dates the name offered first stays;
//! 3. among other names the shorter distance wins, then the smaller
//!    `~<n>` part.
//!
//! These rules reproduce git's names for about 96 % of the commits of
//! git.git (`name-rev --all`); git's choice among names of the same tag and
//! between tags reached through different merges is not fully understood
//! yet (see the comparison in the CLI tests).
//!
//! Distance counts one per first-parent step and [`MERGE_WEIGHT`] per step
//! to another parent, so a name with fewer merge hops always wins. Equal
//! names keep the one offered first: tips are processed in the order the
//! caller gives them, and from each tip the first parent line is followed
//! before the other parents.

use std::collections::HashMap;
use std::rc::Rc;

use rgit_hash::ObjectId;

use crate::walk::{CommitInfo, CommitSource};

/// Distance added for a step to a second or later parent.
pub const MERGE_WEIGHT: u64 = 65535;

/// Commits more than this much older than the oldest commit asked about
/// are not named (they cannot lead to it).
pub const CUTOFF_SLOP: i64 = 24 * 60 * 60;

/// A starting point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tip {
    /// The commit (already peeled).
    pub commit: ObjectId,
    /// The name as printed (`main`, `tags/v1.0`).
    pub name: String,
    /// The reference is a tag (`refs/tags/`).
    pub from_tag: bool,
    /// Tagger date of an annotated tag, else the commit date.
    pub date: i64,
    /// The reference points to a tag object: the commit itself is `<name>^0`.
    pub deref: bool,
}

/// The name of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevName {
    /// The tip the name starts from, including `^<k>` / `~<n>^<k>` steps
    /// through merges.
    pub tip: Rc<str>,
    /// Number of first-parent steps after `tip`.
    pub generation: u64,
    /// Weighted distance from the reference.
    pub distance: u64,
    pub from_tag: bool,
    pub date: i64,
    pub deref: bool,
}

impl RevName {
    /// `tip`, `tip^0` or `tip~<n>`.
    pub fn display(&self) -> String {
        if self.generation > 0 {
            format!("{}~{}", self.tip, self.generation)
        } else if self.deref {
            format!("{}^0", self.tip)
        } else {
            self.tip.to_string()
        }
    }

    /// Whether `self` beats the name `old`.
    pub fn better_than(&self, old: &RevName) -> bool {
        if self.from_tag && old.from_tag {
            return self.date < old.date;
        }
        if self.from_tag != old.from_tag {
            return self.from_tag;
        }
        if self.distance != old.distance {
            return self.distance < old.distance;
        }
        self.generation < old.generation
    }
}

/// Names of commits, filled by [`Namer::name_tip`].
pub struct Namer<'s, S: CommitSource> {
    source: &'s mut S,
    names: HashMap<ObjectId, RevName>,
    commits: HashMap<ObjectId, CommitInfo>,
    cutoff: i64,
    /// Commits in the order they were first read (with their parents).
    pub order: Vec<ObjectId>,
}

impl<'s, S: CommitSource> Namer<'s, S> {
    /// A namer that skips commits older than `cutoff` (committer date).
    pub fn new(source: &'s mut S, cutoff: i64) -> Self {
        Namer {
            source,
            names: HashMap::new(),
            commits: HashMap::new(),
            cutoff,
            order: Vec::new(),
        }
    }

    /// The name of `id`, if it has one.
    pub fn get(&self, id: &ObjectId) -> Option<&RevName> {
        self.names.get(id)
    }

    /// Read a commit (cached).
    pub fn load(&mut self, id: &ObjectId) -> Result<&CommitInfo, S::Error> {
        if !self.commits.contains_key(id) {
            let info = self.source.load(id)?;
            self.order.push(*id);
            self.commits.insert(*id, info);
        }
        Ok(&self.commits[id])
    }

    fn offer(&mut self, id: &ObjectId, name: RevName) -> bool {
        match self.names.get(id) {
            Some(old) if !name.better_than(old) => false,
            _ => {
                self.names.insert(*id, name);
                true
            }
        }
    }

    /// Name the history of `tip`.
    pub fn name_tip(&mut self, tip: &Tip) -> Result<(), S::Error> {
        let time = self.load(&tip.commit)?.time;
        if time < self.cutoff {
            return Ok(());
        }
        let first = RevName {
            tip: Rc::from(tip.name.as_str()),
            generation: 0,
            distance: 0,
            from_tag: tip.from_tag,
            date: tip.date,
            deref: tip.deref,
        };
        if !self.offer(&tip.commit, first) {
            return Ok(());
        }
        let mut stack = vec![tip.commit];
        while let Some(c) = stack.pop() {
            let name = self.names[&c].clone();
            let parents = self.load(&c)?.parents.clone();
            let mut next = Vec::new();
            for (i, p) in parents.iter().enumerate() {
                if self.load(p)?.time < self.cutoff {
                    continue;
                }
                let offered = if i == 0 {
                    RevName {
                        generation: name.generation + 1,
                        distance: name.distance + 1,
                        ..name.clone()
                    }
                } else {
                    let tip = if name.generation > 0 {
                        format!("{}~{}^{}", name.tip, name.generation, i + 1)
                    } else {
                        format!("{}^{}", name.tip, i + 1)
                    };
                    RevName {
                        tip: Rc::from(tip),
                        generation: 0,
                        distance: name.distance + MERGE_WEIGHT,
                        deref: false,
                        ..name.clone()
                    }
                };
                if self.offer(p, offered) {
                    next.push(*p);
                }
            }
            // First parent on top.
            stack.extend(next.into_iter().rev());
        }
        Ok(())
    }
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

    fn graph(edges: &[(u8, &[u8])]) -> Fake {
        Fake(
            edges
                .iter()
                .map(|(n, ps)| {
                    (
                        id(*n),
                        CommitInfo {
                            parents: ps.iter().map(|&p| id(p)).collect(),
                            time: i64::from(*n) * 100,
                        },
                    )
                })
                .collect(),
        )
    }

    fn tip(n: u8, name: &str, from_tag: bool, date: i64, deref: bool) -> Tip {
        Tip {
            commit: id(n),
            name: name.into(),
            from_tag,
            date,
            deref,
        }
    }

    /// A(1) - main(6); M2(5) = A + C(4); C - M1(3) = A + B(2).
    #[test]
    fn merge_parents_get_caret_names() {
        let mut g = graph(&[
            (1, &[]),
            (2, &[1]),
            (3, &[1, 2]),
            (4, &[3]),
            (5, &[1, 4]),
            (6, &[1, 5]),
        ]);
        let mut n = Namer::new(&mut g, 0);
        n.name_tip(&tip(6, "main", false, 600, false)).unwrap();
        assert_eq!(n.get(&id(2)).unwrap().display(), "main^2^2~1^2");
        assert_eq!(n.get(&id(1)).unwrap().display(), "main~1");
    }

    #[test]
    fn tags_beat_branches_and_older_tags_win() {
        let mut g = graph(&[(1, &[]), (2, &[1]), (3, &[2])]);
        let mut n = Namer::new(&mut g, 0);
        n.name_tip(&tip(3, "main", false, 300, false)).unwrap();
        n.name_tip(&tip(3, "tags/new", true, 900, true)).unwrap();
        n.name_tip(&tip(2, "tags/old", true, 500, true)).unwrap();
        assert_eq!(n.get(&id(3)).unwrap().display(), "tags/new^0");
        assert_eq!(n.get(&id(2)).unwrap().display(), "tags/old^0");
        assert_eq!(n.get(&id(1)).unwrap().display(), "tags/old~1");
    }

    #[test]
    fn cutoff_skips_old_commits() {
        let mut g = graph(&[(1, &[]), (2, &[1]), (3, &[2])]);
        let mut n = Namer::new(&mut g, 150);
        n.name_tip(&tip(3, "main", false, 300, false)).unwrap();
        assert!(n.get(&id(1)).is_none());
        assert_eq!(n.get(&id(2)).unwrap().display(), "main~1");
    }
}
