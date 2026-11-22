// SPDX-License-Identifier: MIT
//! Patch equivalence for `rebase` dropping commits that are already
//! upstream (git-rebase(1) `--reapply-cherry-picks`, git-patch-id(1)):
//! two commits are equivalent when they change the same paths in the
//! same way, ignoring whitespace and line numbers.
//!
//! The ids computed here only serve comparison inside one run; they are
//! not the ids `git patch-id` prints.

use std::collections::HashMap;

use rgit_hash::ObjectId;
use rgit_object::{Object, ObjectKind};

use crate::repository::Repo;

/// A mode and blob.
type Blob = Option<(u32, ObjectId)>;

/// One changed path: the path and the old and new blob.
type Change = (Vec<u8>, Blob, Blob);

/// The paths `commit` changes against its first parent (or the empty
/// tree), without rename detection.
pub(crate) fn changes(repo: &Repo, commit: &ObjectId) -> Option<Vec<Change>> {
    let Some(Object::Commit(c)) = repo.read_object(commit) else {
        return None;
    };
    let tree = c.tree().ok()?;
    let parent_tree = match c.parents().unwrap_or_default().first() {
        Some(p) => crate::switch_branch::commit_tree(repo, p)?,
        None => ObjectId::empty_tree(repo.hash),
    };
    let mut out = Vec::new();
    diff_trees(repo, Some(&parent_tree), Some(&tree), b"", &mut out)?;
    Some(out)
}

fn tree_entries(repo: &Repo, tree: Option<&ObjectId>) -> Option<Vec<(Vec<u8>, u32, ObjectId)>> {
    let Some(tree) = tree else {
        return Some(Vec::new());
    };
    if *tree == ObjectId::empty_tree(repo.hash) {
        return Some(Vec::new());
    }
    match repo.read_object(tree) {
        Some(Object::Tree(t)) => Some(
            t.entries
                .into_iter()
                .map(|e| (e.name.to_vec(), e.mode.canonical().bits(), e.oid))
                .collect(),
        ),
        _ => None,
    }
}

const TREE: u32 = 0o040000;

fn is_tree(mode: u32) -> bool {
    mode & 0o170000 == TREE
}

/// Appends the changed blobs between the trees `a` and `b` (recursing only
/// into subtrees that differ).
fn diff_trees(
    repo: &Repo,
    a: Option<&ObjectId>,
    b: Option<&ObjectId>,
    base: &[u8],
    out: &mut Vec<Change>,
) -> Option<()> {
    if a.is_some() && a == b {
        return Some(());
    }
    let ea = tree_entries(repo, a)?;
    let eb = tree_entries(repo, b)?;
    let mut map: HashMap<&[u8], (Blob, Blob)> = HashMap::new();
    for (n, m, o) in &ea {
        map.entry(n.as_slice()).or_default().0 = Some((*m, *o));
    }
    for (n, m, o) in &eb {
        map.entry(n.as_slice()).or_default().1 = Some((*m, *o));
    }
    let mut names: Vec<&[u8]> = map.keys().copied().collect();
    names.sort();
    for name in names {
        let (old, new) = map[name];
        if old == new {
            continue;
        }
        let mut path = base.to_vec();
        path.extend_from_slice(name);
        let old_tree = old.filter(|(m, _)| is_tree(*m)).map(|(_, o)| o);
        let new_tree = new.filter(|(m, _)| is_tree(*m)).map(|(_, o)| o);
        if old_tree.is_some() || new_tree.is_some() {
            let mut sub = path.clone();
            sub.push(b'/');
            diff_trees(repo, old_tree.as_ref(), new_tree.as_ref(), &sub, out)?;
        }
        let old_blob = old.filter(|(m, _)| !is_tree(*m));
        let new_blob = new.filter(|(m, _)| !is_tree(*m));
        if old_blob.is_some() || new_blob.is_some() {
            out.push((path, old_blob, new_blob));
        }
    }
    Some(())
}

/// A cheap key: the changed paths with their modes. Commits with
/// different keys cannot be equivalent.
pub(crate) fn path_key(changes: &[Change]) -> Vec<u8> {
    let mut key = Vec::new();
    for (path, old, new) in changes {
        key.extend_from_slice(path);
        key.push(0);
        for side in [old, new] {
            match side {
                Some((m, _)) => key.extend_from_slice(format!("{m:o}").as_bytes()),
                None => key.push(b'-'),
            }
            key.push(0);
        }
    }
    key
}

/// The full key: [`path_key`] plus every changed line without its
/// whitespace (binary files by their blob ids).
pub(crate) fn patch_key(repo: &Repo, changes: &[Change]) -> Vec<u8> {
    let mut key = path_key(changes);
    let read = |side: &Option<(u32, ObjectId)>| -> Vec<u8> {
        match side {
            Some((_, oid)) => match repo.odb().read(oid) {
                Ok(Some(o)) if o.kind == ObjectKind::Blob => o.data,
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    };
    for (_, old, new) in changes {
        let a = read(old);
        let b = read(new);
        if rgit_diff::is_binary(&a) || rgit_diff::is_binary(&b) {
            for side in [old, new] {
                if let Some((_, o)) = side {
                    key.extend_from_slice(o.as_bytes());
                }
                key.push(0);
            }
            continue;
        }
        let d = rgit_diff::diff_text(&a, &b, &rgit_diff::DiffOptions::default());
        for line in d.hunks.split(|&c| c == b'\n') {
            if line.starts_with(b"@@") {
                continue;
            }
            key.extend(line.iter().filter(|c| !c.is_ascii_whitespace()));
            key.push(b'\n');
        }
    }
    key
}
