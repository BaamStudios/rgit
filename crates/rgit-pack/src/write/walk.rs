// SPDX-License-Identifier: MIT

//! Which objects go into a pack, and in which order.
//!
//! [`enumerate`] lists the objects reachable from some tips and not from
//! others, like `git rev-list --objects <tips> --not <haves>`
//! (`Documentation/git-rev-list.adoc`). It is a small stand-alone walk
//! until the revision walker (#78) exists; it reads everything through an
//! [`ObjectSource`].
//!
//! [`pack_order`] arranges a list of objects the way packs are usually laid
//! out: commits newest first, each annotated tag right after the commit it
//! points to, then trees, then blobs, each group in walk order. Readers of
//! the pack then find the objects of recent history close together.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, Tag, TreeIter};

use super::{ObjectSource, WriteError};
use crate::ObjectKind;

/// One tip of a walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tip {
    /// The object (any type; tags are peeled).
    pub id: ObjectId,
    /// `true` for `^<rev>` / after `--not`: objects reachable from it are
    /// left out.
    pub exclude: bool,
}

/// An object to pack, with what the order needs to know about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListedObject {
    /// The object.
    pub id: ObjectId,
    /// Its type.
    pub kind: ObjectKind,
    /// For an annotated tag: the object it points to.
    pub tag_target: Option<ObjectId>,
}

/// Steps the commit walk goes on once only excluded commits are queued.
const SLOP: u32 = 5;

/// A commit waiting in the walk queue, ordered by committer time (newest
/// first), then by insertion order.
struct Queued {
    time: i64,
    seq: u64,
    id: ObjectId,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time
            .cmp(&other.time)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct ParsedCommit {
    tree: ObjectId,
    parents: Vec<ObjectId>,
    time: i64,
}

struct Walker<'a, S: ObjectSource + ?Sized> {
    source: &'a S,
    hash: HashKind,
    commits: HashMap<ObjectId, ParsedCommit>,
}

impl<S: ObjectSource + ?Sized> Walker<'_, S> {
    fn read(&self, id: &ObjectId) -> Result<crate::Object, WriteError> {
        match self.source.read(id) {
            Ok(Some(o)) => Ok(o),
            Ok(None) => Err(WriteError::Missing(*id)),
            Err(e) => Err(WriteError::read(*id, e)),
        }
    }

    fn read_kind(&self, id: &ObjectId, expected: ObjectKind) -> Result<Vec<u8>, WriteError> {
        let o = self.read(id)?;
        if o.kind != expected {
            return Err(WriteError::WrongType {
                id: *id,
                found: o.kind,
                expected,
            });
        }
        Ok(o.data)
    }

    fn commit(&mut self, id: &ObjectId) -> Result<&ParsedCommit, WriteError> {
        if !self.commits.contains_key(id) {
            let data = self.read_kind(id, ObjectKind::Commit)?;
            let corrupt = || WriteError::Corrupt {
                id: *id,
                kind: ObjectKind::Commit,
            };
            let c = Commit::parse(self.hash, &data).map_err(|_| corrupt())?;
            let time = c
                .committer()
                .and_then(|s| s.time())
                .map_or(0, |t| t.seconds);
            let parsed = ParsedCommit {
                tree: c.tree().map_err(|_| corrupt())?,
                parents: c.parents().map_err(|_| corrupt())?,
                time,
            };
            self.commits.insert(*id, parsed);
        }
        Ok(&self.commits[id])
    }

    /// Peel tags. Returns the tags passed on the way and the final object.
    fn peel(&self, id: ObjectId) -> Result<(Vec<ListedObject>, ObjectId, ObjectKind), WriteError> {
        let mut tags = Vec::new();
        let mut cur = id;
        loop {
            let o = self.read(&cur)?;
            if o.kind != ObjectKind::Tag {
                return Ok((tags, cur, o.kind));
            }
            let target = Tag::parse(self.hash, &o.data)
                .and_then(|t| t.object())
                .map_err(|_| WriteError::Corrupt {
                    id: cur,
                    kind: ObjectKind::Tag,
                })?;
            tags.push(ListedObject {
                id: cur,
                kind: ObjectKind::Tag,
                tag_target: Some(target),
            });
            if tags.len() > 10_000 {
                return Err(WriteError::Corrupt {
                    id: cur,
                    kind: ObjectKind::Tag,
                });
            }
            cur = target;
        }
    }

    /// Add `root` and everything below it to `seen`, appending the objects
    /// not seen before to `out` (pre-order, entries in tree order).
    /// Submodule commits (gitlinks) are not followed.
    fn walk_tree(
        &self,
        root: ObjectId,
        seen: &mut HashSet<ObjectId>,
        mut out: Option<&mut Vec<ListedObject>>,
    ) -> Result<(), WriteError> {
        if !seen.insert(root) {
            return Ok(());
        }
        let push = |out: &mut Option<&mut Vec<ListedObject>>, id, kind| {
            if let Some(o) = out.as_deref_mut() {
                o.push(ListedObject {
                    id,
                    kind,
                    tag_target: None,
                });
            }
        };
        push(&mut out, root, ObjectKind::Tree);
        // Depth-first with an explicit stack of (tree content, position).
        let mut stack: Vec<(ObjectId, Vec<u8>, usize)> =
            vec![(root, self.read_kind(&root, ObjectKind::Tree)?, 0)];
        while let Some((tree_id, data, pos)) = stack.last_mut() {
            let entry = match TreeIter::new(self.hash, &data[*pos..]).next() {
                None => {
                    stack.pop();
                    continue;
                }
                Some(Err(_)) => {
                    return Err(WriteError::Corrupt {
                        id: *tree_id,
                        kind: ObjectKind::Tree,
                    });
                }
                Some(Ok(e)) => e,
            };
            *pos += entry.mode_digits.len() + entry.name.len() + 2 + self.hash.byte_len();
            let id = entry.oid;
            match entry.mode_bits & 0o170000 {
                0o160000 => {}
                0o040000 => {
                    if seen.insert(id) {
                        push(&mut out, id, ObjectKind::Tree);
                        let sub = self.read_kind(&id, ObjectKind::Tree)?;
                        stack.push((id, sub, 0));
                    }
                }
                _ => {
                    if seen.insert(id) {
                        push(&mut out, id, ObjectKind::Blob);
                    }
                }
            }
        }
        Ok(())
    }
}

/// List the objects reachable from the tips with `exclude == false` and
/// not reachable from those with `exclude == true`, in `rev-list
/// --objects` order: commits newest first, then the tags, trees and blobs
/// named as tips, then the trees and blobs of each commit.
///
/// Like `git rev-list`, trees and blobs are left out when they are
/// reachable from the tree of an excluded parent of a listed commit (and
/// from excluded trees and blobs named as tips); objects that the
/// excluded side has only elsewhere in its history are still listed. The
/// result was checked against `git rev-list --objects` for random
/// (wants, haves) pairs in git's own history.
pub fn enumerate<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    tips: &[Tip],
) -> Result<Vec<ListedObject>, WriteError> {
    let mut w = Walker {
        source,
        hash,
        commits: HashMap::new(),
    };

    // Sort the tips by type after peeling tags.
    let mut include_commits = Vec::new();
    let mut exclude_commits = Vec::new();
    let mut pending: Vec<ListedObject> = Vec::new(); // tags, trees, blobs named directly
    let mut excluded_roots: Vec<(ObjectId, ObjectKind)> = Vec::new();
    for tip in tips {
        let (tags, id, kind) = w.peel(tip.id)?;
        if tip.exclude {
            match kind {
                ObjectKind::Commit => exclude_commits.push(id),
                _ => excluded_roots.push((id, kind)),
            }
            // Excluded tags themselves are not sent either.
            excluded_roots.extend(tags.iter().map(|t| (t.id, ObjectKind::Tag)));
            continue;
        }
        pending.extend(tags);
        match kind {
            ObjectKind::Commit => include_commits.push(id),
            _ => pending.push(ListedObject {
                id,
                kind,
                tag_target: None,
            }),
        }
    }

    // The commit walk: one queue for both sides, newest first. Exclusion
    // spreads from excluded commits to their parents; the walk ends as
    // soon as only excluded commits are left in the queue.
    let mut excluded: HashSet<ObjectId> = HashSet::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut in_queue: HashSet<ObjectId> = HashSet::new();
    let mut queue = BinaryHeap::new();
    let mut seq = 0u64;
    let mut included_in_queue = 0usize;
    for (ids, exclude) in [(&exclude_commits, true), (&include_commits, false)] {
        for id in ids.iter() {
            if exclude {
                mark_excluded(&w, *id, &mut excluded, &in_queue, &mut included_in_queue);
            }
            if seen.insert(*id) {
                let time = w.commit(id)?.time;
                queue.push(Queued { time, seq, id: *id });
                seq += 1;
                in_queue.insert(*id);
                if !excluded.contains(id) {
                    included_in_queue += 1;
                }
            }
        }
    }
    let mut popped = Vec::new();
    let mut slop = SLOP;
    while let Some(Queued { id, time, .. }) = queue.pop() {
        in_queue.remove(&id);
        let is_excluded = excluded.contains(&id);
        if !is_excluded {
            included_in_queue -= 1;
            popped.push(id);
        }
        let parents = w.commit(&id)?.parents.clone();
        for p in parents {
            if is_excluded {
                mark_excluded(&w, p, &mut excluded, &in_queue, &mut included_in_queue);
            }
            if seen.insert(p) {
                let time = w.commit(&p)?.time;
                queue.push(Queued { time, seq, id: p });
                seq += 1;
                in_queue.insert(p);
                if !excluded.contains(&p) {
                    included_in_queue += 1;
                }
            }
        }
        if !is_excluded {
            continue;
        }
        // Only excluded commits left: go on for a few more steps, in case
        // a commit with a wrong (too old) date hides an excluded path.
        let Some(head) = queue.peek() else {
            break;
        };
        if time <= head.time || included_in_queue > 0 {
            slop = SLOP;
            continue;
        }
        slop -= 1;
        if slop == 0 {
            break;
        }
    }
    let commits: Vec<ObjectId> = popped
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .collect();

    // Objects the other side has: everything in the trees of the excluded
    // parents of the listed commits (the edge of the excluded history).
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut edges = HashSet::new();
    for id in &commits {
        for p in w.commit(id)?.parents.clone() {
            if excluded.contains(&p) && edges.insert(p) {
                let tree = w.commit(&p)?.tree;
                w.walk_tree(tree, &mut seen, None)?;
            }
        }
    }
    for (id, kind) in excluded_roots {
        match kind {
            ObjectKind::Tree => w.walk_tree(id, &mut seen, None)?,
            _ => {
                seen.insert(id);
            }
        }
    }

    let mut out: Vec<ListedObject> = Vec::with_capacity(commits.len() * 4);
    let mut listed: HashSet<ObjectId> = HashSet::new();
    for id in &commits {
        out.push(ListedObject {
            id: *id,
            kind: ObjectKind::Commit,
            tag_target: None,
        });
    }
    let mut trees_from_tips = Vec::new();
    for o in pending {
        match o.kind {
            ObjectKind::Tree => trees_from_tips.push(o.id),
            _ => {
                if !seen.contains(&o.id) && listed.insert(o.id) {
                    out.push(o);
                }
            }
        }
    }
    for t in trees_from_tips {
        w.walk_tree(t, &mut seen, Some(&mut out))?;
    }
    for id in &commits {
        let tree = w.commit(id)?.tree;
        w.walk_tree(tree, &mut seen, Some(&mut out))?;
    }
    Ok(out)
}

/// Rearrange `objects` into pack order: commits, each followed by the tags
/// pointing at it, then the remaining tags, trees and blobs; the relative
/// order inside each group is kept. Duplicates are dropped.
pub fn pack_order(objects: &[ListedObject]) -> Vec<ListedObject> {
    let mut seen = HashSet::new();
    let unique: Vec<&ListedObject> = objects.iter().filter(|o| seen.insert(o.id)).collect();
    let commits: HashSet<ObjectId> = unique
        .iter()
        .filter(|o| o.kind == ObjectKind::Commit)
        .map(|o| o.id)
        .collect();
    // Tags of tags count for the commit at the end of the chain, after the
    // tags they point to.
    let tag_targets: HashMap<ObjectId, ObjectId> = unique
        .iter()
        .filter(|o| o.kind == ObjectKind::Tag)
        .filter_map(|o| o.tag_target.map(|t| (o.id, t)))
        .collect();
    let peel = |mut id: ObjectId| {
        let mut hops = 0usize;
        while let Some(t) = tag_targets.get(&id) {
            id = *t;
            hops += 1;
            if hops > tag_targets.len() {
                break;
            }
        }
        (id, hops)
    };
    let mut tags_of: HashMap<ObjectId, Vec<(usize, &ListedObject)>> = HashMap::new();
    let mut other_tags = Vec::new();
    for o in unique.iter().filter(|o| o.kind == ObjectKind::Tag) {
        let (target, hops) = peel(o.id);
        if commits.contains(&target) {
            tags_of.entry(target).or_default().push((hops, o));
        } else {
            other_tags.push(*o);
        }
    }
    for tags in tags_of.values_mut() {
        tags.sort_by_key(|(hops, _)| *hops);
    }
    let mut out = Vec::with_capacity(unique.len());
    for o in unique.iter().filter(|o| o.kind == ObjectKind::Commit) {
        out.push(**o);
        if let Some(tags) = tags_of.get(&o.id) {
            out.extend(tags.iter().map(|(_, t)| **t));
        }
    }
    out.extend(other_tags.into_iter().copied());
    for kind in [ObjectKind::Tree, ObjectKind::Blob] {
        out.extend(unique.iter().filter(|o| o.kind == kind).map(|o| **o));
    }
    out
}

/// Mark `id` excluded, and the parents of every newly excluded commit
/// whose parents are known.
fn mark_excluded<S: ObjectSource + ?Sized>(
    w: &Walker<'_, S>,
    id: ObjectId,
    excluded: &mut HashSet<ObjectId>,
    in_queue: &HashSet<ObjectId>,
    included_in_queue: &mut usize,
) {
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if !excluded.insert(id) {
            continue;
        }
        if in_queue.contains(&id) {
            *included_in_queue -= 1;
        }
        if let Some(c) = w.commits.get(&id) {
            stack.extend(c.parents.iter().copied());
        }
    }
}

/// Look up the type (and for tags the target) of each id, for packing a
/// plain object list. Unknown ids are an error.
pub fn describe<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    ids: &[ObjectId],
) -> Result<Vec<ListedObject>, WriteError> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let kind = match source.kind(id) {
            Ok(Some(k)) => k,
            Ok(None) => return Err(WriteError::Missing(*id)),
            Err(e) => return Err(WriteError::read(*id, e)),
        };
        let tag_target = if kind == ObjectKind::Tag {
            match source.read(id) {
                Ok(Some(o)) => Tag::parse(hash, &o.data).and_then(|t| t.object()).ok(),
                Ok(None) => return Err(WriteError::Missing(*id)),
                Err(e) => return Err(WriteError::read(*id, e)),
            }
        } else {
            None
        };
        out.push(ListedObject {
            id: *id,
            kind,
            tag_target,
        });
    }
    Ok(out)
}
