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
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, Tag, TreeIter};

use super::filter::Compiled;
use super::{ObjectFilter, ObjectSource, WriteError};
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
    /// [`name_hash`] of the path the object was found under (0: none).
    /// Objects with equal or similar names are compared in the delta
    /// search.
    pub name_hash: u32,
}

impl ListedObject {
    /// An object without a path.
    pub fn new(id: ObjectId, kind: ObjectKind) -> ListedObject {
        ListedObject {
            id,
            kind,
            tag_target: None,
            name_hash: 0,
        }
    }
}

/// The sort key of a path for the delta search: equal for equal file
/// names (in any directory), close for names that end alike. The high 16
/// bits are the last two bytes of the name (so `*.c` files sort near each
/// other), the low 16 bits a hash of the whole file name. Whitespace is
/// ignored; the empty path gives 0.
pub fn name_hash(path: &[u8]) -> u32 {
    let base = match path.iter().rposition(|&b| b == b'/') {
        Some(slash) if slash + 1 < path.len() => &path[slash + 1..],
        Some(_) => {
            let trimmed = &path[..path.len() - 1];
            return name_hash(trimmed);
        }
        None => path,
    };
    let mut tail = [0u8; 2];
    let mut n = 0;
    let mut h: u32 = 0x811c_9dc5;
    for &b in base.iter().filter(|b| !b.is_ascii_whitespace()) {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        tail = [tail[1], b];
        n += 1;
    }
    if n == 0 {
        return 0;
    }
    (u32::from(tail[1]) << 24) | (u32::from(tail[0]) << 16) | ((h ^ (h >> 16)) & 0xffff)
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
        let o = match self.read(id) {
            // git: `bad tree object <id>` for a tree that is not there.
            Err(WriteError::Missing(_)) if expected == ObjectKind::Tree => {
                return Err(WriteError::Corrupt {
                    id: *id,
                    kind: ObjectKind::Tree,
                });
            }
            other => other?,
        };
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
                // A shallow commit has no parents here.
                parents: if self.source.is_shallow(id) {
                    Vec::new()
                } else {
                    c.parents().map_err(|_| corrupt())?
                },
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
            // The final object is not read: it may be a large blob.
            if let Ok(Some((kind, _))) = self.source.header(&cur) {
                if kind != ObjectKind::Tag {
                    return Ok((tags, cur, kind));
                }
            }
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
                tag_target: Some(target),
                ..ListedObject::new(cur, ObjectKind::Tag)
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

    /// The entries of a tree: name, id, and whether it is a tree.
    /// Submodule commits (gitlinks) are left out.
    fn tree_entries(&self, id: &ObjectId) -> Result<Vec<(Vec<u8>, ObjectId, bool)>, WriteError> {
        let data = self.read_kind(id, ObjectKind::Tree)?;
        let mut out = Vec::new();
        for entry in TreeIter::new(self.hash, &data) {
            let entry = entry.map_err(|_| WriteError::Corrupt {
                id: *id,
                kind: ObjectKind::Tree,
            })?;
            match entry.mode_bits & 0o170000 {
                0o160000 => {}
                0o040000 => out.push((entry.name.to_vec(), entry.oid, true)),
                _ => out.push((entry.name.to_vec(), entry.oid, false)),
            }
        }
        Ok(out)
    }

    /// The sparse walk of the excluded side: starting with the root trees,
    /// add the entries of the `excluded` trees to `seen` (and `edges`),
    /// and descend only into names where an `included` tree has a tree
    /// not seen yet and an excluded tree has a tree too.
    fn mark_sparse(
        &self,
        included: Vec<ObjectId>,
        excluded: Vec<ObjectId>,
        seen: &mut HashSet<ObjectId>,
        mut edges: Option<&mut Vec<ListedObject>>,
    ) -> Result<(), WriteError> {
        let mut mark = |seen: &mut HashSet<ObjectId>, id: ObjectId, kind, name: &[u8]| {
            if seen.insert(id) {
                if let Some(e) = edges.as_deref_mut() {
                    e.push(ListedObject {
                        name_hash: name_hash(name),
                        ..ListedObject::new(id, kind)
                    });
                }
            }
        };
        for t in &excluded {
            mark(seen, *t, ObjectKind::Tree, b"");
        }
        let mut groups = vec![(included, excluded)];
        while let Some((inc, exc)) = groups.pop() {
            type Sides = (Vec<ObjectId>, Vec<ObjectId>);
            let mut by_name: BTreeMap<Vec<u8>, Sides> = BTreeMap::new();
            let mut done: HashSet<ObjectId> = HashSet::new();
            for t in exc.iter().filter(|t| done.insert(**t)) {
                // What the other side has need not be complete here.
                let Ok(entries) = self.tree_entries(t) else {
                    continue;
                };
                for (name, id, is_tree) in entries {
                    let kind = if is_tree {
                        ObjectKind::Tree
                    } else {
                        ObjectKind::Blob
                    };
                    mark(seen, id, kind, &name);
                    if is_tree {
                        by_name.entry(name).or_default().1.push(id);
                    }
                }
            }
            let mut done: HashSet<ObjectId> = HashSet::new();
            for t in inc
                .iter()
                .filter(|t| !seen.contains(*t) && done.insert(**t))
            {
                for (name, id, is_tree) in self.tree_entries(t)? {
                    if is_tree && !seen.contains(&id) {
                        if let Some(sides) = by_name.get_mut(&name) {
                            sides.0.push(id);
                        }
                    }
                }
            }
            for (_, (inc, exc)) in by_name {
                if !inc.is_empty() && !exc.is_empty() {
                    groups.push((inc, exc));
                }
            }
        }
        Ok(())
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
        let push = |out: &mut Option<&mut Vec<ListedObject>>, id, kind, name: &[u8]| {
            if let Some(o) = out.as_deref_mut() {
                o.push(ListedObject {
                    name_hash: name_hash(name),
                    ..ListedObject::new(id, kind)
                });
            }
        };
        push(&mut out, root, ObjectKind::Tree, b"");
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
                        push(&mut out, id, ObjectKind::Tree, entry.name);
                        let sub = self.read_kind(&id, ObjectKind::Tree)?;
                        stack.push((id, sub, 0));
                    }
                }
                _ => {
                    if seen.insert(id) {
                        push(&mut out, id, ObjectKind::Blob, entry.name);
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
    walk_objects(source, hash, tips, false, false, None)
}

/// Like [`enumerate`], and also returns the trees and blobs of the edge:
/// the excluded parents of listed commits, and excluded trees named as
/// tips. The other side has them, so they can serve as delta bases of a
/// thin pack ([`write_thin_pack`](super::write_thin_pack)).
///
/// With `sparse` (`pack-objects --sparse`, `pack.useSparse`) the trees of
/// excluded parents are only walked along the paths where the listed
/// commits have trees that differ from them: an object that the other
/// side has only under another path is listed again (a copied directory,
/// for example), but the walk stays small for a small change in a big
/// tree. Only the objects seen on those paths are returned as edge.
pub fn enumerate_with_edges<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    tips: &[Tip],
    sparse: bool,
) -> Result<(Vec<ListedObject>, Vec<ListedObject>), WriteError> {
    let mut edges = Vec::new();
    let listed = walk_objects(source, hash, tips, sparse, false, Some(&mut edges))?;
    Ok((listed, edges))
}

/// Like [`enumerate_with_edges`] without `sparse`, but the edge also holds
/// the trees of the excluded tips, not only those of the excluded parents
/// of listed commits (`rev-list
/// --objects-edge-aggressive`, used for thin packs from shallow
/// repositories, where the parents are often cut off). Trees and blobs the
/// edge holds are not listed.
pub fn enumerate_with_edges_aggressive<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    tips: &[Tip],
) -> Result<(Vec<ListedObject>, Vec<ListedObject>), WriteError> {
    let mut edges = Vec::new();
    let listed = walk_objects(source, hash, tips, false, true, Some(&mut edges))?;
    Ok((listed, edges))
}

/// How [`enumerate_with`] walks.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnumerateOptions<'a> {
    /// See [`enumerate_with_edges`].
    pub sparse: bool,
    /// See [`enumerate_with_edges_aggressive`].
    pub aggressive: bool,
    /// Leave out the objects this filter does not accept (`--filter`).
    /// The tips themselves are always listed. The edge then only holds
    /// what a reader that got its objects through the same filter has:
    /// trees for the filters that keep all trees, else nothing.
    pub filter: Option<&'a ObjectFilter>,
    /// Tips that nobody named: they come from a set of references
    /// (`--all`, `--reflog`, ...). The filter applies to the trees and
    /// blobs among them and behind their tags, as it does not to the
    /// tips given one by one.
    pub unnamed_tips: &'a [ObjectId],
}

/// [`enumerate_with_edges`] with all options: the listed objects and the
/// edge.
pub fn enumerate_with<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    tips: &[Tip],
    opts: &EnumerateOptions<'_>,
) -> Result<(Vec<ListedObject>, Vec<ListedObject>), WriteError> {
    let filter = match opts.filter {
        Some(f) => Some(f.compile().map_err(WriteError::Filter)?),
        None => None,
    };
    let mut edges = Vec::new();
    let listed = walk_objects_filtered(
        source,
        hash,
        tips,
        opts.sparse,
        opts.aggressive,
        Some(&mut edges),
        filter.as_ref(),
        opts.unnamed_tips,
    )?;
    if let Some(f) = &filter {
        let trees = f.keeps_all_trees();
        edges.retain(|e| trees && e.kind == ObjectKind::Tree);
    }
    Ok((listed, edges))
}

fn walk_objects<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    tips: &[Tip],
    sparse: bool,
    aggressive: bool,
    edge_objects: Option<&mut Vec<ListedObject>>,
) -> Result<Vec<ListedObject>, WriteError> {
    walk_objects_filtered(
        source,
        hash,
        tips,
        sparse,
        aggressive,
        edge_objects,
        None,
        &[],
    )
}

/// What the filtered tree walk has done so far.
#[derive(Default)]
struct FilterState {
    /// Objects in the output.
    listed: HashSet<ObjectId>,
    /// Blobs the filter left out. A blob named as a tip after a tree
    /// that holds it is not listed either (as `rev-list` behaves).
    omitted: HashSet<ObjectId>,
    /// The smallest depth each tree was expanded at.
    expanded: HashMap<ObjectId, u64>,
}

impl<S: ObjectSource + ?Sized> Walker<'_, S> {
    /// [`Walker::walk_tree`] with a filter: list `root` and what is below
    /// it as far as `filter` accepts it. `seen` holds what the reader has
    /// and is not changed.
    ///
    /// A `provided` root (a tree named as a tip) is listed whatever the
    /// filter says, and for `tree:<depth>` its entries have depth 0, like
    /// the root tree of a commit.
    fn walk_tree_filtered(
        &self,
        root: ObjectId,
        provided: bool,
        filter: &Compiled,
        seen: &HashSet<ObjectId>,
        state: &mut FilterState,
        out: &mut Vec<ListedObject>,
    ) -> Result<(), WriteError> {
        fn push(
            state: &mut FilterState,
            out: &mut Vec<ListedObject>,
            id: ObjectId,
            kind: ObjectKind,
            name: &[u8],
        ) {
            if state.listed.insert(id) {
                out.push(ListedObject {
                    name_hash: name_hash(name),
                    ..ListedObject::new(id, kind)
                });
            }
        }
        // Depths here are one more than the filter's: a provided tree is
        // at 0, the root tree of a commit at 1. Without a depth limit
        // every tree is expanded once, at "depth 0".
        let limit = filter.tree_depth;
        let step = u64::from(limit.is_some());
        let too_deep = |depth: u64| limit.is_some_and(|d| depth > d);
        let wants_blobs = filter.allows(ObjectKind::Blob) && filter.blob_limit != Some(0);
        // Trees are only read for what they hold.
        let descends = filter.allows(ObjectKind::Tree) || wants_blobs;
        // Enter a tree: list it and say whether to look at its entries.
        // Entries that are all too deep are not looked at, except with
        // `tree:0`, as `rev-list` behaves (see `FilterState::omitted`).
        let enter = |state: &mut FilterState,
                     out: &mut Vec<ListedObject>,
                     id: ObjectId,
                     name: &[u8],
                     depth: u64,
                     provided: bool|
         -> bool {
            if seen.contains(&id) || too_deep(depth) || !(provided || descends) {
                return false;
            }
            match state.expanded.get(&id) {
                Some(&prev) if prev <= depth => return false,
                _ => state.expanded.insert(id, depth),
            };
            if provided || filter.allows(ObjectKind::Tree) {
                push(state, out, id, ObjectKind::Tree, name);
            }
            limit == Some(0) || !too_deep(depth + 1)
        };
        let root_depth = if provided { 0 } else { step };
        if !enter(state, out, root, b"", root_depth, provided) {
            return Ok(());
        }
        let mut stack: Vec<(ObjectId, Vec<u8>, usize, u64)> = vec![(
            root,
            self.read_kind(&root, ObjectKind::Tree)?,
            0,
            root_depth,
        )];
        while let Some((tree_id, data, pos, depth)) = stack.last_mut() {
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
            let child_depth = *depth + step;
            match entry.mode_bits & 0o170000 {
                0o160000 => {}
                0o040000 => {
                    let name = entry.name.to_vec();
                    if enter(state, out, id, &name, child_depth, false) {
                        let sub = self.read_kind(&id, ObjectKind::Tree)?;
                        stack.push((id, sub, 0, child_depth));
                    }
                }
                _ => {
                    if seen.contains(&id) || state.listed.contains(&id) {
                        continue;
                    }
                    if !wants_blobs || too_deep(child_depth) {
                        state.omitted.insert(id);
                        continue;
                    }
                    if let Some(max) = filter.blob_limit {
                        let size = match self.source.header(&id) {
                            Ok(Some((_, size))) => size,
                            Ok(None) => return Err(WriteError::Missing(id)),
                            Err(e) => return Err(WriteError::read(id, e)),
                        };
                        if size >= max {
                            state.omitted.insert(id);
                            continue;
                        }
                    }
                    let name = entry.name.to_vec();
                    push(state, out, id, ObjectKind::Blob, &name);
                }
            }
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_objects_filtered<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    tips: &[Tip],
    sparse: bool,
    aggressive: bool,
    mut edge_objects: Option<&mut Vec<ListedObject>>,
    filter: Option<&Compiled>,
    unnamed_tips: &[ObjectId],
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
    // Trees and blobs from tips nobody named: a filter applies to them.
    let unnamed_tips: HashSet<ObjectId> = unnamed_tips.iter().copied().collect();
    let mut unnamed: HashSet<ObjectId> = HashSet::new();
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
        if unnamed_tips.contains(&tip.id) {
            unnamed.insert(id);
        } else {
            unnamed.remove(&id);
        }
        pending.extend(tags);
        match kind {
            ObjectKind::Commit => include_commits.push(id),
            _ => pending.push(ListedObject::new(id, kind)),
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
                // Missing objects on the excluded side are ignored (the
                // other side has them; we may have only part of them).
                let time = match w.commit(id) {
                    Ok(c) => c.time,
                    Err(_) if exclude => continue,
                    Err(e) => return Err(e),
                };
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
                let time = match w.commit(&p) {
                    Ok(c) => c.time,
                    Err(_) if excluded.contains(&p) => continue,
                    Err(e) => return Err(e),
                };
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
    let mut edge_trees = Vec::new();
    for id in &commits {
        for p in w.commit(id)?.parents.clone() {
            if excluded.contains(&p) && edges.insert(p) {
                // A shallow history may lack the excluded parent.
                if let Ok(c) = w.commit(&p) {
                    let tree = c.tree;
                    if sparse {
                        edge_trees.push(tree);
                    } else {
                        let _ = w.walk_tree(tree, &mut seen, edge_objects.as_deref_mut());
                    }
                }
            }
        }
    }
    // Aggressive: the trees of the excluded tips themselves (the other
    // side has them even when it lacks their parents).
    let tips_edge: &[ObjectId] = if aggressive { &exclude_commits } else { &[] };
    for &id in tips_edge {
        if edges.insert(id) {
            if let Ok(c) = w.commit(&id) {
                let tree = c.tree;
                let _ = w.walk_tree(tree, &mut seen, edge_objects.as_deref_mut());
            }
        }
    }
    if sparse && !edge_trees.is_empty() {
        let mut included = Vec::with_capacity(commits.len());
        for id in &commits {
            included.push(w.commit(id)?.tree);
        }
        w.mark_sparse(included, edge_trees, &mut seen, edge_objects.as_deref_mut())?;
    }
    for (id, kind) in excluded_roots {
        match kind {
            ObjectKind::Tree => w.walk_tree(id, &mut seen, edge_objects.as_deref_mut())?,
            _ => {
                seen.insert(id);
            }
        }
    }

    let mut out: Vec<ListedObject> = Vec::with_capacity(commits.len() * 4);
    let mut listed: HashSet<ObjectId> = HashSet::new();
    if let Some(filter) = filter {
        // The tips are listed whatever the filter says.
        let provided: HashSet<ObjectId> = include_commits.iter().copied().collect();
        let all_commits = filter.allows(ObjectKind::Commit);
        for id in &commits {
            if all_commits || provided.contains(id) {
                out.push(ListedObject::new(*id, ObjectKind::Commit));
            }
        }
        // The objects named as tips, in their order. A tree or blob from
        // a tip nobody named goes through the filter like the root tree
        // (or a file in the root tree) of a commit.
        let mut state = FilterState::default();
        for o in pending {
            let named = !unnamed.contains(&o.id);
            match o.kind {
                ObjectKind::Tree => {
                    w.walk_tree_filtered(o.id, named, filter, &seen, &mut state, &mut out)?;
                }
                ObjectKind::Blob if !named => {
                    let mut wanted = filter.allows(ObjectKind::Blob)
                        && filter.tree_depth != Some(0)
                        && !seen.contains(&o.id);
                    if let (true, Some(max)) = (wanted, filter.blob_limit) {
                        wanted = match w.source.header(&o.id) {
                            Ok(Some((_, size))) => size < max,
                            Ok(None) => return Err(WriteError::Missing(o.id)),
                            Err(e) => return Err(WriteError::read(o.id, e)),
                        };
                    }
                    if wanted && state.listed.insert(o.id) {
                        out.push(o);
                    }
                }
                _ => {
                    if !seen.contains(&o.id)
                        && !state.omitted.contains(&o.id)
                        && state.listed.insert(o.id)
                    {
                        out.push(o);
                    }
                }
            }
        }
        if filter.walks_trees() {
            for id in &commits {
                let tree = w.commit(id)?.tree;
                w.walk_tree_filtered(tree, false, filter, &seen, &mut state, &mut out)?;
            }
        }
        return Ok(out);
    }
    for id in &commits {
        out.push(ListedObject::new(*id, ObjectKind::Commit));
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
            tag_target,
            ..ListedObject::new(*id, kind)
        });
    }
    Ok(out)
}
