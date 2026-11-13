// SPDX-License-Identifier: MIT
//! Commit graph helpers: commit headers for negotiation, the
//! fast-forward test and the connectivity check after a transfer.

use std::collections::{BinaryHeap, HashSet};

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Tag, TreeIter};
use rgit_odb::ObjectDatabase;
use rgit_protocol::v2::CommitInfo;

/// Parents and committer time of the commit `id`; `None` if it is missing
/// or not a commit.
pub fn commit_info(odb: &ObjectDatabase, id: &ObjectId) -> Option<CommitInfo> {
    let raw = odb.read(id).ok()??;
    if raw.kind != ObjectKind::Commit {
        return None;
    }
    let c = Commit::parse(odb.hash_kind(), &raw.data).ok()?;
    let time = c
        .committer()
        .and_then(|s| s.time())
        .map(|t| t.seconds)
        .unwrap_or(0);
    Some(CommitInfo {
        time,
        parents: c.parents().ok()?,
    })
}

/// Commits with equal or older dates than the target that are still
/// walked before giving up (clock skew allowance).
const SLOP: usize = 5;

/// Whether commit `old` is an ancestor of (or equal to) commit `new`.
///
/// The history of `new` is walked newest first; the walk stops when `old`
/// is found, or when only commits older than `old` are left and a few more
/// were looked at without finding it.
pub fn is_ancestor(odb: &ObjectDatabase, old: &ObjectId, new: &ObjectId) -> bool {
    if old == new {
        return true;
    }
    let Some(old_info) = commit_info(odb, old) else {
        return false;
    };
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut queue: BinaryHeap<(i64, ObjectId)> = BinaryHeap::new();
    let Some(info) = commit_info(odb, new) else {
        return false;
    };
    seen.insert(*new);
    queue.push((info.time, *new));
    let mut slop = SLOP;
    while let Some((time, id)) = queue.pop() {
        if id == *old {
            return true;
        }
        if time < old_info.time {
            if slop == 0 {
                return false;
            }
            slop -= 1;
        }
        let Some(info) = commit_info(odb, &id) else {
            continue;
        };
        for p in info.parents {
            if seen.insert(p) {
                if let Some(pi) = commit_info(odb, &p) {
                    queue.push((pi.time, p));
                }
            }
        }
    }
    false
}

/// Check that everything reachable from `tips` exists locally.
///
/// Objects of the received pack (`received`) are opened and their links
/// followed; objects that existed before the transfer are taken to be
/// complete, as the repository's own invariant guarantees. Returns the
/// first missing object.
pub fn check_connected(
    odb: &ObjectDatabase,
    tips: &[ObjectId],
    received: &HashSet<ObjectId>,
) -> Result<(), ObjectId> {
    let hash = odb.hash_kind();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<(ObjectId, bool)> = tips.iter().map(|t| (*t, false)).collect();
    while let Some((id, gitlink)) = stack.pop() {
        if gitlink || !seen.insert(id) {
            continue;
        }
        if !received.contains(&id) {
            if odb.contains(&id) {
                continue;
            }
            return Err(id);
        }
        let Ok(Some(raw)) = odb.read(&id) else {
            return Err(id);
        };
        match raw.kind {
            ObjectKind::Commit => {
                if let Ok(c) = Commit::parse(hash, &raw.data) {
                    if let Ok(t) = c.tree() {
                        stack.push((t, false));
                    }
                    for p in c.parents().unwrap_or_default() {
                        stack.push((p, false));
                    }
                }
            }
            ObjectKind::Tree => {
                for e in TreeIter::new(hash, &raw.data).flatten() {
                    // Submodule commits (mode 160000) are not ours to have.
                    let is_gitlink = e.mode_bits & 0o170000 == 0o160000;
                    stack.push((e.oid, is_gitlink));
                }
            }
            ObjectKind::Tag => {
                if let Ok(t) = Tag::parse(hash, &raw.data) {
                    if let Ok(o) = t.object() {
                        stack.push((o, false));
                    }
                }
            }
            ObjectKind::Blob => {}
        }
    }
    Ok(())
}

/// Follow tags from `id` to the first non-tag object (for local objects).
pub fn peel(odb: &ObjectDatabase, id: &ObjectId) -> Option<ObjectId> {
    let mut cur = *id;
    for _ in 0..64 {
        let raw = odb.read(&cur).ok()??;
        if raw.kind != ObjectKind::Tag {
            return Some(cur);
        }
        cur = Tag::parse(odb.hash_kind(), &raw.data).ok()?.object().ok()?;
    }
    None
}
