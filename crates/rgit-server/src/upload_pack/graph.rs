// SPDX-License-Identifier: MIT
//! The commit graph as `upload-pack` walks it: parents and commit times,
//! read from the object database and remembered, with the repository's
//! own shallow commits cut off.

use std::collections::{HashMap, HashSet};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, ObjectKind, Tag};
use rgit_odb::ObjectDatabase;

/// What the walks need to know about a commit.
#[derive(Debug, Clone)]
pub(crate) struct CommitInfo {
    /// The parents; none for a commit the repository has only shallowly.
    pub parents: Vec<ObjectId>,
    /// Committer time.
    pub time: i64,
}

/// Commits of a repository, read on demand.
pub(crate) struct Graph<'a> {
    odb: &'a ObjectDatabase,
    hash: HashKind,
    grafts: HashSet<ObjectId>,
    commits: HashMap<ObjectId, Option<CommitInfo>>,
}

impl<'a> Graph<'a> {
    /// The commits in `odb`; those in `shallow` have no parents.
    pub fn new(odb: &'a ObjectDatabase, hash: HashKind, shallow: &[ObjectId]) -> Graph<'a> {
        Graph {
            odb,
            hash,
            grafts: shallow.iter().copied().collect(),
            commits: HashMap::new(),
        }
    }

    /// Whether the repository has `id` only shallowly.
    pub fn is_graft(&self, id: &ObjectId) -> bool {
        self.grafts.contains(id)
    }

    /// The commit `id`; `None` if it does not exist, is no commit or
    /// cannot be parsed.
    pub fn commit(&mut self, id: &ObjectId) -> Option<&CommitInfo> {
        if !self.commits.contains_key(id) {
            let info = self.read(id);
            self.commits.insert(*id, info);
        }
        self.commits.get(id).and_then(Option::as_ref)
    }

    fn read(&self, id: &ObjectId) -> Option<CommitInfo> {
        let raw = self.odb.read(id).ok().flatten()?;
        if raw.kind != ObjectKind::Commit {
            return None;
        }
        let commit = Commit::parse(self.hash, &raw.data).ok()?;
        let time = commit
            .committer()
            .and_then(|s| s.time())
            .map_or(0, |t| t.seconds);
        let parents = if self.grafts.contains(id) {
            Vec::new()
        } else {
            commit.parents().ok()?
        };
        Some(CommitInfo { parents, time })
    }

    /// Follow tags from `id` to the object they name; `None` when an
    /// object on the way is missing or broken.
    pub fn peel(&self, id: &ObjectId) -> Option<(ObjectId, ObjectKind)> {
        let mut cur = *id;
        for _ in 0..=64 {
            let header = self.odb.header(&cur).ok().flatten()?;
            if header.kind != ObjectKind::Tag {
                return Some((cur, header.kind));
            }
            let raw = self.odb.read(&cur).ok().flatten()?;
            cur = Tag::parse(self.hash, &raw.data)
                .and_then(|t| t.object())
                .ok()?;
        }
        None
    }

    /// `id` peeled to a commit, if it names one.
    pub fn peel_to_commit(&self, id: &ObjectId) -> Option<ObjectId> {
        match self.peel(id)? {
            (commit, ObjectKind::Commit) => Some(commit),
            _ => None,
        }
    }
}
