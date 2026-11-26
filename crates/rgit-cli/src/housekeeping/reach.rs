// SPDX-License-Identifier: MIT
//! Reachability for `repack` and `prune`.
//!
//! The starting points ([`Roots`]) are what `git-prune(1)` and
//! `git-repack(1)` describe: every reference of every worktree (including
//! the per-worktree ones such as `refs/bisect/*`), the `HEAD` of every
//! worktree, every entry of every reflog, and the objects named by the
//! index of every worktree (entries, cache-tree trees, resolve-undo
//! blobs), and the original `HEAD` and autostash of a rebase in progress.
//! Root references such as `ORIG_HEAD` or `MERGE_HEAD` are not
//! starting points (checked against the reference git).
//!
//! [`Walk`] marks everything reachable from a set of starting points:
//! commits by committer date (newest first), then the trees and blobs of
//! each commit; gitlinks are not followed and commits listed in
//! `.git/shallow` have no parents. A broken reference, or an object
//! missing on the way from a reference, is fatal: deleting objects with
//! an incomplete picture of what is reachable would lose data. Reflog
//! and index entries whose objects are gone are skipped, as git does.

use std::collections::{BinaryHeap, HashSet};
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_index::{Index, ReadOptions};
use rgit_object::{Commit, ObjectKind, Tag, TreeIter};
use rgit_odb::ObjectDatabase;
use rgit_pack::write::ListedObject;
use rgit_refs::files::FilesStore;
use rgit_refs::reflog::FilesReflog;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_repo::Setup;

use crate::Die;
use crate::messages::housekeeping as msg;
use crate::worktrees;

/// A reference that cannot be used as a starting point.
#[derive(Debug)]
pub enum BadRef {
    /// The file does not hold a reference value.
    Unparsable(String),
    /// The object it names does not exist.
    Missing(String),
}

/// Where a walk starts.
#[derive(Debug, Default)]
pub struct Roots {
    /// Values of references and `HEAD`s, with their names.
    pub refs: Vec<(String, ObjectId)>,
    /// Reflog and index entries (skipped when their object is gone).
    pub weak: Vec<ObjectId>,
}

/// Collect the starting points of the repository.
pub fn roots(setup: &Setup, odb: &ObjectDatabase) -> Result<Roots, BadRef> {
    let kind = setup.format.object_format;
    let common = setup.absolute_common_dir();
    let mut out = Roots::default();
    let mut seen_refs: HashSet<String> = HashSet::new();
    // GIT_REF_PARANOIA (git(1)): on unless set to false.
    let paranoia = std::env::var("GIT_REF_PARANOIA")
        .map(|v| {
            !matches!(
                v.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    for wt in worktrees::list(setup, false) {
        let store = FilesStore::new(&common, kind).with_worktree(&wt.git_dir);
        let linked = wt.id.is_some();
        // HEAD of the worktree.
        match store.read_raw("HEAD") {
            Ok(Some(_)) => {
                if let Ok(Some(id)) = store.resolve_oid("HEAD") {
                    out.refs.push(("HEAD".to_string(), id));
                }
            }
            Ok(None) => {}
            Err(_) => return Err(BadRef::Unparsable("HEAD".to_string())),
        }
        let items = store
            .iter(&IterOptions::default())
            .map_err(|_| BadRef::Unparsable("refs/".to_string()))?;
        for item in items {
            let name = item.name().to_string();
            let per_worktree = rgit_refs::name::is_per_worktree(&name);
            // Shared references are the same in every worktree.
            let key = if linked && per_worktree {
                format!("{}\0{name}", wt.git_dir.display())
            } else {
                name.clone()
            };
            if !seen_refs.insert(key) {
                continue;
            }
            match item {
                IterItem::Ref(r) => {
                    if let Some(id) = r.oid {
                        out.refs.push((name, id));
                    }
                }
                // A broken file or a name git would not create: stop,
                // unless GIT_REF_PARANOIA=0 says to skip such references.
                IterItem::Broken(_) | IterItem::BadName(_) if paranoia => {
                    return Err(BadRef::Unparsable(name));
                }
                IterItem::Broken(_) | IterItem::BadName(_) => {}
            }
        }
        reflog_objects(&wt.git_dir, &common, kind, linked, &mut out.weak);
        // A rebase in progress keeps its original HEAD and autostash.
        for dir in ["rebase-merge", "rebase-apply"] {
            for file in ["orig-head", "autostash"] {
                if let Ok(text) = std::fs::read_to_string(wt.git_dir.join(dir).join(file)) {
                    if let Ok(id) = ObjectId::from_hex(kind, text.trim().as_bytes()) {
                        out.weak.push(id);
                    }
                }
            }
        }
        index_objects(&worktree_index(&wt), kind, &mut out.weak);
    }
    if paranoia {
        if let Some((name, _)) = out.refs.iter().find(|(_, id)| !odb.contains(id)) {
            return Err(BadRef::Missing(name.clone()));
        }
    } else {
        out.refs.retain(|(_, id)| odb.contains(id));
    }
    Ok(out)
}

/// The objects of every reflog entry of every worktree, without null
/// ids (`rev-list --reflog`, `pack-objects --reflog`).
pub fn all_reflog_objects(setup: &Setup) -> Vec<ObjectId> {
    let kind = setup.format.object_format;
    let common = setup.absolute_common_dir();
    let mut out = Vec::new();
    for wt in worktrees::list(setup, false) {
        reflog_objects(&wt.git_dir, &common, kind, wt.id.is_some(), &mut out);
    }
    out
}

/// The objects named by the index of every worktree (`--indexed-objects`).
pub fn all_index_objects(setup: &Setup) -> Vec<ObjectId> {
    let kind = setup.format.object_format;
    let mut out = Vec::new();
    for wt in worktrees::list(setup, false) {
        index_objects(&worktree_index(&wt), kind, &mut out);
    }
    out
}

fn worktree_index(wt: &worktrees::Worktree) -> PathBuf {
    if wt.current {
        rgit_index::index_path(&wt.git_dir)
    } else {
        wt.git_dir.join("index")
    }
}

/// Reflogs: the shared ones once (from the main worktree), the
/// per-worktree ones of each worktree.
fn reflog_objects(
    git_dir: &Path,
    common: &Path,
    kind: HashKind,
    linked: bool,
    out: &mut Vec<ObjectId>,
) {
    let logs = FilesReflog::new(git_dir, common, kind);
    let Ok(names) = logs.list() else { return };
    for name in names {
        let per_worktree = rgit_refs::name::is_per_worktree(&name);
        if linked && !per_worktree {
            continue;
        }
        if let Ok(Some(entries)) = logs.read(&name) {
            for e in entries {
                for id in [e.old, e.new] {
                    if !id.is_null() {
                        out.push(id);
                    }
                }
            }
        }
    }
}

/// The objects an index file names.
fn index_objects(path: &Path, kind: HashKind, out: &mut Vec<ObjectId>) {
    let Ok(index) = Index::read_file(path, kind, &ReadOptions::default()) else {
        return;
    };
    for e in index.entries() {
        if !e.mode.is_gitlink() {
            out.push(e.id);
        }
    }
    if let Some(tree) = index.cache_tree() {
        let mut stack = vec![&tree.root];
        while let Some(node) = stack.pop() {
            if let Some(id) = node.id() {
                out.push(*id);
            }
            stack.extend(node.children.iter());
        }
    }
    // The blobs of the resolve undo records (not gitlinks).
    for r in index.resolve_undo() {
        for (mode, id) in r.stages.iter().flatten() {
            if mode & 0o170000 != 0o160000 {
                out.push(*id);
            }
        }
    }
}

/// The commits listed in `<git-dir>/shallow`.
pub fn shallow_commits(common: &Path, kind: HashKind) -> HashSet<ObjectId> {
    read_shallow(&shallow_path(common), kind)
}

/// Path of the shallow file.
pub fn shallow_path(common: &Path) -> PathBuf {
    common.join("shallow")
}

/// Drop the commits that no longer exist from `.git/shallow`; remove the
/// file when none is left.
pub fn prune_shallow(odb: &ObjectDatabase, common: &Path, kind: HashKind) {
    let path = shallow_path(common);
    let Ok(data) = std::fs::read(&path) else {
        return;
    };
    let mut kept = Vec::new();
    let mut changed = false;
    for line in data.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        match ObjectId::from_hex(kind, line) {
            Ok(id) if !odb.contains(&id) => changed = true,
            _ => {
                kept.extend_from_slice(line);
                kept.push(b'\n');
            }
        }
    }
    if !changed {
        return;
    }
    if kept.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, kept);
    }
}

fn read_shallow(path: &Path, kind: HashKind) -> HashSet<ObjectId> {
    let Ok(data) = std::fs::read(path) else {
        return HashSet::new();
    };
    data.split(|&b| b == b'\n')
        .filter_map(|l| ObjectId::from_hex(kind, l).ok())
        .collect()
}

/// A commit waiting in the walk, newest first.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    time: i64,
    seq: std::cmp::Reverse<u64>,
    id: ObjectId,
    tree: ObjectId,
    parents: Vec<ObjectId>,
}

/// Marks reachable objects.
pub struct Walk<'a> {
    odb: &'a ObjectDatabase,
    hash: HashKind,
    shallow: HashSet<ObjectId>,
    /// Everything marked so far.
    pub seen: HashSet<ObjectId>,
    /// The marked objects in walk order: per call the commits newest
    /// first and the tags, then trees and blobs.
    pub listed: Vec<ListedObject>,
    seq: u64,
}

impl<'a> Walk<'a> {
    pub fn new(odb: &'a ObjectDatabase, shallow: HashSet<ObjectId>) -> Walk<'a> {
        Walk {
            odb,
            hash: odb.hash_kind(),
            shallow,
            seen: HashSet::new(),
            listed: Vec::new(),
            seq: 0,
        }
    }

    fn read(&self, id: &ObjectId) -> Option<rgit_object::RawObject> {
        self.odb.read(id).ok().flatten()
    }

    fn list(&mut self, id: ObjectId, kind: ObjectKind, tag_target: Option<ObjectId>) {
        self.listed.push(ListedObject {
            id,
            kind,
            tag_target,
            // No path: the delta search orders these by type and size.
            name_hash: 0,
        });
    }

    fn queue_commit(&mut self, id: ObjectId, data: &[u8]) -> Result<Queued, Die> {
        let c = Commit::parse(self.hash, data)
            .map_err(|_| Die::Fatal(msg::bad_object(&id.to_hex())))?;
        let tree = c
            .tree()
            .map_err(|_| Die::Fatal(msg::bad_object(&id.to_hex())))?;
        let parents = if self.shallow.contains(&id) {
            Vec::new()
        } else {
            c.parents()
                .map_err(|_| Die::Fatal(msg::bad_object(&id.to_hex())))?
        };
        let time = c
            .committer()
            .and_then(|s| s.time())
            .map_or(0, |t| t.seconds);
        self.seq += 1;
        Ok(Queued {
            time,
            seq: std::cmp::Reverse(self.seq),
            id,
            tree,
            parents,
        })
    }

    /// Mark everything reachable from `tips`. With `lenient`, missing
    /// objects are skipped; otherwise they are fatal.
    pub fn walk(&mut self, tips: &[ObjectId], lenient: bool) -> Result<(), Die> {
        let mut queue: BinaryHeap<Queued> = BinaryHeap::new();
        let mut trees: Vec<ObjectId> = Vec::new();
        for &tip in tips {
            let mut id = tip;
            loop {
                if !self.seen.insert(id) {
                    break;
                }
                // Blobs are listed without reading them (they may be
                // large).
                if let Ok(Some(h)) = self.odb.header(&id) {
                    if h.kind == ObjectKind::Blob {
                        self.list(id, ObjectKind::Blob, None);
                        break;
                    }
                }
                let Some(o) = self.read(&id) else {
                    self.seen.remove(&id);
                    if lenient {
                        break;
                    }
                    return Err(Die::Fatal(msg::unable_to_read(&id.to_hex())));
                };
                match o.kind {
                    ObjectKind::Commit => {
                        let q = self.queue_commit(id, &o.data)?;
                        queue.push(q);
                        break;
                    }
                    ObjectKind::Tree => {
                        self.seen.remove(&id);
                        trees.push(id);
                        break;
                    }
                    ObjectKind::Blob => {
                        self.list(id, ObjectKind::Blob, None);
                        break;
                    }
                    ObjectKind::Tag => {
                        let target = Tag::parse(self.hash, &o.data)
                            .and_then(|t| t.object())
                            .map_err(|_| Die::Fatal(msg::bad_object(&id.to_hex())))?;
                        self.list(id, ObjectKind::Tag, Some(target));
                        id = target;
                    }
                }
            }
        }
        let mut commit_trees = Vec::new();
        while let Some(q) = queue.pop() {
            self.list(q.id, ObjectKind::Commit, None);
            commit_trees.push(q.tree);
            for p in q.parents {
                if !self.seen.insert(p) {
                    continue;
                }
                let parsed = match self.read(&p) {
                    Some(o) if o.kind == ObjectKind::Commit => Some(self.queue_commit(p, &o.data)?),
                    _ => None,
                };
                match parsed {
                    Some(pq) => queue.push(pq),
                    None if lenient => {
                        self.seen.remove(&p);
                    }
                    None => {
                        eprintln!("error: {}", msg::could_not_read(&p.to_hex()));
                        return Err(Die::Fatal(msg::failed_to_traverse(&q.id.to_hex())));
                    }
                }
            }
        }
        for t in commit_trees.into_iter().chain(trees) {
            self.walk_tree(t, lenient)?;
        }
        Ok(())
    }

    /// Mark `root` and everything below it (pre-order).
    fn walk_tree(&mut self, root: ObjectId, lenient: bool) -> Result<(), Die> {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !self.seen.insert(id) {
                continue;
            }
            let data = match self.read(&id) {
                Some(o) if o.kind == ObjectKind::Tree => o.data,
                _ if lenient => {
                    self.seen.remove(&id);
                    continue;
                }
                _ => return Err(Die::Fatal(msg::unable_to_read_tree(&id.to_hex()))),
            };
            self.list(id, ObjectKind::Tree, None);
            let mut subtrees = Vec::new();
            for entry in TreeIter::new(self.hash, &data) {
                let Ok(e) = entry else {
                    if lenient {
                        break;
                    }
                    return Err(Die::Fatal(msg::bad_object(&id.to_hex())));
                };
                match e.mode_bits & 0o170000 {
                    0o160000 => {}
                    0o040000 => subtrees.push(e.oid),
                    _ => {
                        if self.seen.insert(e.oid) {
                            self.list(e.oid, ObjectKind::Blob, None);
                        }
                    }
                }
            }
            // Visit the subtrees in tree order.
            stack.extend(subtrees.into_iter().rev());
        }
        Ok(())
    }
}
