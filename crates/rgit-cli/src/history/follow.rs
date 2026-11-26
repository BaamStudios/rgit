// SPDX-License-Identifier: MIT
//! `log --follow` (Documentation/git-log.adoc): the history of one file
//! beyond renames and copies.
//!
//! Observed behaviour of the `git` binary, which this follows:
//!
//! - the walk itself is not limited by the path: every commit of the
//!   range is walked (TREESAME plays no role, side branches of merges that
//!   kept the other version are walked too), in the usual order;
//! - commits are filtered as they come out of the walk (after `--skip`
//!   and `--reverse`, before `-n`): each commit has a tracked path, the
//!   given path unless a child passed another one on. A commit with at
//!   most one parent is shown when its tracked path differs from the
//!   parent's; merges are never shown;
//! - when the tracked path does not exist in a parent, the parent's
//!   files are searched for its source (identical content first, then the
//!   most similar file with at least 50 % similarity, renames before
//!   copies); the parent then tracks the source's path. Every other parent
//!   tracks the same path. A commit no rename passed a path to tracks the
//!   source of the last rename found in a commit that is not a merge (so
//!   `--reverse` and `--skip` lose track, as in git);
//! - a pathspec with wildcards is not followed: commits that change a
//!   matching path are shown.

use std::collections::HashMap;

use rgit_diff::diffcore::{DEFAULT_RENAME_SCORE, similarity::Pieces, similarity::similarity};
use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, TreeIter};
use rgit_pathspec::Pathspec;
use rgit_revwalk::revision::ObjectLookup;

use super::Repo;

/// One file of a tree: mode and blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub mode: u32,
    pub id: ObjectId,
}

/// The `--follow` filter.
pub struct Follow {
    /// The given path, relative to the top.
    path: Vec<u8>,
    /// The pathspec when it has wildcards (then nothing is followed).
    pattern: Option<Pathspec>,
    /// Paths passed on to parents.
    tracked: HashMap<ObjectId, Vec<u8>>,
    /// The path of commits nothing was passed to.
    current: Vec<u8>,
    /// The path the commit [`Follow::show`] looked at last renamed or
    /// copied its tracked file from (commits with one parent).
    pub source: Option<Vec<u8>>,
    /// Changed-path filters of the commit-graph (set up on first use).
    bloom: Option<Option<crate::commit_graph::BloomFilter>>,
}

impl Follow {
    /// Follow `path` (relative to the top). `pattern` is the parsed
    /// pathspec when it is not a plain path.
    pub fn new(path: Vec<u8>, pattern: Option<Pathspec>) -> Follow {
        Follow {
            current: path.clone(),
            path,
            pattern,
            tracked: HashMap::new(),
            source: None,
            bloom: None,
        }
    }

    /// The path commit `id` is followed by.
    pub fn path_of(&self, id: &ObjectId) -> &[u8] {
        if self.pattern.is_some() {
            return &self.path;
        }
        self.tracked.get(id).map_or(&self.current, Vec::as_slice)
    }

    /// Whether to show commit `id` (with the walk's `parents`); passes the
    /// tracked path on to the parents.
    pub fn show(
        &mut self,
        repo: &Repo<'_>,
        id: &ObjectId,
        parents: &[ObjectId],
    ) -> Result<bool, ObjectId> {
        let tree = repo.tree_of(id).ok_or(*id)?;
        self.source = None;
        if let Some(pattern) = &self.pattern {
            if parents.len() > 1 {
                return Ok(false);
            }
            let old = match parents.first() {
                Some(p) => Some(repo.tree_of(p).ok_or(*p)?),
                None => None,
            };
            let mut new_files = HashMap::new();
            let mut old_files = HashMap::new();
            files(repo, Some(&tree), b"", &mut new_files)?;
            files(repo, old.as_ref(), b"", &mut old_files)?;
            let changed = new_files
                .iter()
                .filter(|(p, e)| old_files.get(*p) != Some(e))
                .map(|(p, _)| p)
                .chain(old_files.keys().filter(|p| !new_files.contains_key(*p)))
                .any(|p| pattern.matches(p));
            return Ok(changed);
        }
        let path = self.path_of(id).to_vec();
        if parents.len() <= 1 {
            // The filter of a commit with at most one parent tells whether
            // the tracked path changed at all.
            let bloom = self.bloom.get_or_insert_with(|| {
                repo.objects
                    .commit_graph()
                    .and_then(|g| crate::commit_graph::BloomFilter::for_path(g, &self.path))
            });
            if let Some(b) = bloom {
                b.set_path(&path);
                if b.check(repo.objects.commit_graph(), id, parents.first())
                    == crate::commit_graph::BloomAnswer::DefinitelyNot
                {
                    return Ok(false);
                }
            }
        }
        let mine = entry_at(repo, &tree, &path)?;
        if parents.is_empty() {
            return Ok(mine.is_some());
        }
        let mut differs = false;
        for p in parents {
            let ptree = repo.tree_of(p).ok_or(*p)?;
            let theirs = entry_at(repo, &ptree, &path)?;
            if theirs != mine {
                differs = true;
                if theirs.is_none()
                    && let Some(m) = mine
                    && let Some(source) = find_source(repo, &tree, &ptree, &m)?
                {
                    if parents.len() == 1 {
                        self.current.clone_from(&source);
                        self.source = Some(source.clone());
                    }
                    self.tracked.entry(*p).or_insert(source);
                }
            }
        }
        Ok(differs && parents.len() == 1)
    }
}

/// The entry at `path` in `tree`.
pub(super) fn entry_at(
    repo: &Repo<'_>,
    tree: &ObjectId,
    path: &[u8],
) -> Result<Option<Entry>, ObjectId> {
    let mut tree = *tree;
    let mut parts = path.split(|&b| b == b'/').peekable();
    while let Some(name) = parts.next() {
        let (kind, data) = repo.objects.read(&tree).ok().flatten().ok_or(tree)?;
        if kind != ObjectKind::Tree {
            return Ok(None);
        }
        let mut found = None;
        for e in TreeIter::new(repo.objects.hash_kind(), &data) {
            let e = e.map_err(|_| tree)?;
            if e.name == name {
                found = Some(Entry {
                    mode: e.mode_bits,
                    id: e.oid,
                });
                break;
            }
        }
        let Some(found) = found else {
            return Ok(None);
        };
        if parts.peek().is_none() {
            return Ok(Some(found));
        }
        if found.mode & 0o170000 != 0o040000 {
            return Ok(None);
        }
        tree = found.id;
    }
    Ok(None)
}

/// All files of `tree` (recursively) with their paths below `base`.
pub(super) fn files(
    repo: &Repo<'_>,
    tree: Option<&ObjectId>,
    base: &[u8],
    out: &mut HashMap<Vec<u8>, Entry>,
) -> Result<(), ObjectId> {
    let Some(tree) = tree else {
        return Ok(());
    };
    let (kind, data) = repo.objects.read(tree).ok().flatten().ok_or(*tree)?;
    if kind != ObjectKind::Tree {
        return Err(*tree);
    }
    for e in TreeIter::new(repo.objects.hash_kind(), &data) {
        let e = e.map_err(|_| *tree)?;
        let mut path = base.to_vec();
        path.extend_from_slice(e.name);
        if e.mode_bits & 0o170000 == 0o040000 {
            path.push(b'/');
            files(repo, Some(&e.oid), &path, out)?;
        } else {
            out.insert(
                path,
                Entry {
                    mode: e.mode_bits,
                    id: e.oid,
                },
            );
        }
    }
    Ok(())
}

fn blob(repo: &Repo<'_>, id: &ObjectId) -> Option<Vec<u8>> {
    match repo.objects.read(id).ok()?? {
        (ObjectKind::Blob, data) => Some(data),
        _ => None,
    }
}

/// The path in `old` that the file `target` of `new` was renamed or
/// copied from.
fn find_source(
    repo: &Repo<'_>,
    new: &ObjectId,
    old: &ObjectId,
    target: &Entry,
) -> Result<Option<Vec<u8>>, ObjectId> {
    if target.mode & 0o170000 == 0o160000 {
        return Ok(None);
    }
    let mut old_files = HashMap::new();
    files(repo, Some(old), b"", &mut old_files)?;
    let mut new_files = HashMap::new();
    files(repo, Some(new), b"", &mut new_files)?;
    // Renames (the source is gone) before copies, then by path.
    let mut cands: Vec<(&Vec<u8>, &Entry)> = old_files
        .iter()
        .filter(|(_, e)| e.mode & 0o170000 != 0o160000)
        .collect();
    cands.sort_by_key(|(p, _)| (new_files.contains_key(*p), (*p).clone()));
    if let Some((p, _)) = cands.iter().find(|(_, e)| e.id == target.id) {
        return Ok(Some((*p).clone()));
    }
    let Some(data) = blob(repo, &target.id) else {
        return Ok(None);
    };
    let dst = Pieces::new(&data);
    let mut best: Option<(u32, &Vec<u8>)> = None;
    for (p, e) in cands {
        let Some(src) = blob(repo, &e.id) else {
            continue;
        };
        let score = similarity(&Pieces::new(&src), &dst, DEFAULT_RENAME_SCORE);
        if score >= DEFAULT_RENAME_SCORE && best.is_none_or(|(b, _)| score > b) {
            best = Some((score, p));
        }
    }
    Ok(best.map(|(_, p)| p.clone()))
}
