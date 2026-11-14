// SPDX-License-Identifier: MIT

//! One- and two-way merges of trees into the index (planning) and the work
//! tree update that carries them out.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Index, Stage};
use rgit_worktree::ignore::Excludes;
use rgit_worktree::stat::{Checker, Status, stat_from_metadata};

use crate::Error;
use crate::cache_tree::repair_cache_tree;
use crate::checkout::{self, Filter, join, lossy};
use crate::messages as msg;
use crate::objects::{Objects, read_blob, tree_entries};
use crate::path::{PathOptions, verify_checkout_path};

/// The trees to merge into the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    /// `read-tree -m <tree>` (or `--reset <tree>`): the index becomes
    /// `tree`; entries that do not change keep their stat data.
    OneWay(ObjectId),
    /// `read-tree -m <old> <new>`: move from `old` (what the index and work
    /// tree are based on) to `new`, carrying local changes forward
    /// following the "Two Tree Merge" table of git-read-tree(1).
    TwoWay {
        /// The tree the current index is derived from (`HEAD`).
        old: ObjectId,
        /// The tree to move to.
        new: ObjectId,
    },
}

/// The work tree the merge is checked against and written to.
#[derive(Clone, Copy, Debug)]
pub struct Worktree<'a> {
    /// Top of the work tree.
    pub root: &'a Path,
    /// Compares index entries with their files; its options give
    /// `core.symlinks` for writing.
    pub checker: &'a Checker,
}

/// How to merge (the `read-tree` switches).
#[derive(Debug, Default)]
pub struct Options<'a> {
    /// `--reset`: unmerged entries are dropped, and with `update` local
    /// changes and untracked files are overwritten instead of refusing.
    /// (Two-way entries that match neither tree are still refused.)
    pub reset: bool,
    /// Accept an index with conflicts without `reset`: every conflicted
    /// path takes the new tree's version (or goes away), overwriting its
    /// work tree file, while the other paths keep their checks
    /// (`reset --merge`, `merge --abort`; git-reset(1), DISCUSSION).
    pub discard_unmerged: bool,
    /// `-u`: plan work tree updates and refuse to overwrite untracked files.
    pub update: bool,
    /// `-i`: do not compare the index with the work tree at all.
    pub index_only: bool,
    /// Collect every problem instead of stopping at the first (porcelain
    /// commands, which print them grouped).
    pub show_all_errors: bool,
    /// The index has no entries and no file (`git clone`, a first checkout):
    /// two-way case 3 then takes the new tree.
    pub initial_checkout: bool,
    /// `core.protectNTFS`, `core.protectHFS`.
    pub paths: PathOptions,
    /// Ignored files that may be overwritten or removed (git uses the
    /// standard excludes whenever the work tree is updated).
    pub excludes: Option<&'a mut Excludes>,
}

/// Why the merge would lose local work. Ordered as git groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Problem {
    /// The index entry matches neither tree (or a staged removal).
    WouldOverwrite,
    /// The work tree file differs from its index entry.
    NotUptodateFile,
    /// A directory to be replaced holds untracked files.
    NotUptodateDir,
    /// An untracked file is in the way of a new entry.
    UntrackedOverwritten,
}

/// Which porcelain command's wording to use for [`Rejection::porcelain_messages`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Porcelain {
    /// `checkout`, `switch`.
    Checkout,
    /// `merge`.
    Merge,
}

/// The problems that made the merge refuse, in the order found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rejection {
    problems: Vec<(Problem, Vec<u8>)>,
}

impl Rejection {
    /// Every problem with its path.
    pub fn problems(&self) -> &[(Problem, Vec<u8>)] {
        &self.problems
    }

    /// `read-tree` style: one message per problem (print each after
    /// `error: `; read-tree stops at the first, exit code 128).
    pub fn plumbing_messages(&self) -> Vec<String> {
        self.problems
            .iter()
            .map(|(p, path)| {
                let path = lossy(path);
                match p {
                    Problem::WouldOverwrite => msg::would_overwrite(&path),
                    Problem::NotUptodateFile => msg::not_uptodate_file(&path),
                    Problem::NotUptodateDir => msg::not_uptodate_dir(&path),
                    Problem::UntrackedOverwritten => msg::untracked_overwritten(&path),
                }
            })
            .collect()
    }

    /// Porcelain style: one block per kind of problem, each listing its
    /// paths (print each after `error: `, then [`msg::ABORTING`]).
    pub fn porcelain_messages(&self, cmd: Porcelain) -> Vec<String> {
        let mut kinds: Vec<Problem> = self.problems.iter().map(|(p, _)| *p).collect();
        kinds.sort();
        kinds.dedup();
        kinds
            .into_iter()
            .map(|kind| {
                let (head, tail) = match (kind, cmd) {
                    (Problem::WouldOverwrite | Problem::NotUptodateFile, Porcelain::Checkout) => {
                        msg::CHECKOUT_LOCAL_CHANGES
                    }
                    (Problem::WouldOverwrite | Problem::NotUptodateFile, Porcelain::Merge) => {
                        msg::MERGE_LOCAL_CHANGES
                    }
                    (Problem::NotUptodateDir, _) => msg::DIRS_WITH_UNTRACKED,
                    (Problem::UntrackedOverwritten, Porcelain::Checkout) => {
                        msg::CHECKOUT_UNTRACKED_OVERWRITTEN
                    }
                    (Problem::UntrackedOverwritten, Porcelain::Merge) => {
                        msg::MERGE_UNTRACKED_OVERWRITTEN
                    }
                };
                let mut s = head.to_string();
                for (p, path) in &self.problems {
                    if *p == kind {
                        s.push('\t');
                        s.push_str(&lossy(path));
                        s.push('\n');
                    }
                }
                s.push_str(tail);
                s
            })
            .collect()
    }
}

/// The result of [`plan`]: the new index (new entries without stat data)
/// and the work tree changes that [`Plan::apply`] makes.
#[derive(Clone, Debug)]
pub struct Plan {
    index: Index,
    remove: Vec<(Vec<u8>, bool)>,
    update: Vec<Vec<u8>>,
}

/// What [`Plan::apply`] did.
#[derive(Debug)]
pub struct Applied {
    /// The new index with fresh stat data for the written entries.
    pub index: Index,
    /// Messages (without `error: `) for entries that could not be written
    /// or removed; git reports these and fails with exit code 128 after
    /// writing the index.
    pub errors: Vec<String>,
}

impl Plan {
    /// The merged index (stat data of new entries not yet filled in).
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// The merged index, for callers that do not update the work tree.
    pub fn into_index(self) -> Index {
        self.index
    }

    /// Paths whose work tree files will be written, in index order.
    pub fn updates(&self) -> &[Vec<u8>] {
        &self.update
    }

    /// Paths whose work tree files will be removed, in index order.
    pub fn removals(&self) -> impl Iterator<Item = &[u8]> {
        self.remove.iter().map(|(p, _)| p.as_slice())
    }

    /// Carries out the work tree changes: first every removal (pruning
    /// directories that become empty), then every update, in index order.
    /// Written entries get the `lstat` data of their new files.
    pub fn apply(self, odb: &dyn Objects, wt: &Worktree<'_>, filter: &dyn Filter) -> Applied {
        let Plan {
            mut index,
            remove,
            update,
        } = self;
        let mut errors = Vec::new();
        for (path, gitlink) in &remove {
            if let Err(e) = checkout::remove_entry(wt.root, path, *gitlink) {
                errors.push(msg::unable_to_unlink(&lossy(path), &checkout::strerror(&e)));
            }
        }
        let symlinks = wt.checker.options().symlinks;
        for path in &update {
            let Ok(pos) = index.position(path, Stage::MERGED) else {
                continue;
            };
            let entry = index.entries()[pos].clone();
            let data = if entry.mode.is_gitlink() {
                Vec::new()
            } else {
                match read_blob(odb, &entry.id) {
                    Ok(d) if entry.mode.is_file() => filter.to_worktree(path, &entry.id, d),
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(e.to_string());
                        continue;
                    }
                }
            };
            match checkout::write_entry(wt.root, path, &entry, &data, symlinks) {
                Ok(Some(meta)) => {
                    if let Some(e) = index.entry_mut(pos) {
                        e.stat = stat_from_metadata(&meta);
                    }
                }
                Ok(None) => {}
                Err(e) => errors.push(e.to_string()),
            }
        }
        Applied { index, errors }
    }
}

/// Merges `merge` into `index` and checks that no local change or
/// untracked file is lost. Without `wt` (a bare repository, or `-i`) the
/// work tree is neither checked nor updated.
///
/// # Errors
/// [`Error::Unmerged`] when the index has conflicts (unless `reset`),
/// [`Error::Rejected`] with the problems found, [`Error::InvalidPath`] for
/// a tree path that may not be checked out, and object errors.
pub fn plan(
    odb: &dyn Objects,
    index: &Index,
    merge: &Merge,
    wt: Option<&Worktree<'_>>,
    opts: &mut Options<'_>,
) -> Result<Plan, Error> {
    if !opts.reset && !opts.discard_unmerged && index.has_conflicts() {
        return Err(Error::Unmerged);
    }
    let (old, new) = match *merge {
        Merge::OneWay(t) => (None, tree_entries(odb, &t)?),
        Merge::TwoWay { old, new } => (Some(tree_entries(odb, &old)?), tree_entries(odb, &new)?),
    };
    let wt = if opts.index_only { None } else { wt };
    let mut u = Unpacker {
        index,
        wt,
        opts,
        result: Vec::with_capacity(new.len()),
        update: Vec::new(),
        remove: Vec::new(),
        removed: HashSet::new(),
        consumed: HashSet::new(),
        rejection: Rejection::default(),
    };
    let groups: Vec<&[Entry]> = index
        .entries()
        .chunk_by(|a, b| a.path() == b.path())
        .collect();
    let empty: Vec<Entry> = Vec::new();
    let old_list = old.as_ref().unwrap_or(&empty);
    let (mut gi, mut oi, mut ni) = (0, 0, 0);
    loop {
        let heads = [
            groups.get(gi).map(|g| g[0].path()),
            old_list.get(oi).map(Entry::path),
            new.get(ni).map(Entry::path),
        ];
        let Some(path) = heads.iter().flatten().min().map(|p| p.to_vec()) else {
            break;
        };
        let group = take(&groups, &mut gi, |g| g[0].path(), &path);
        let h = take(old_list, &mut oi, Entry::path, &path);
        let m = take(&new, &mut ni, Entry::path, &path);
        let mut unmerged = false;
        let i = match group {
            Some(g) if !u.consumed.contains(&path) => {
                let stage0 = g.iter().find(|e| e.stage() == Stage::MERGED);
                if g.iter().any(|e| e.stage().is_conflict()) {
                    unmerged = true;
                    None
                } else {
                    stage0
                }
            }
            _ => None,
        };
        let ok = if old.is_some() {
            u.two_way(&path, i, h, m, unmerged)?
        } else {
            u.one_way(&path, i, m, unmerged)?
        };
        if !ok && !u.opts.show_all_errors {
            break;
        }
    }
    if !u.rejection.problems.is_empty() {
        return Err(Error::Rejected(u.rejection));
    }
    let mut out = index.clone();
    out.retain(|_| false);
    out.set_cache_tree(None);
    let add = AddOptions {
        ok_to_replace: true,
        skip_df_check: false,
        skip_path_check: true,
    };
    for e in u.result {
        out.add(e, add).map_err(|e| Error::Io(e.to_string()))?;
    }
    let tree = repair_cache_tree(&out, odb);
    out.set_cache_tree(Some(tree));
    Ok(Plan {
        index: out,
        remove: u.remove,
        update: u.update,
    })
}

/// The item of `list` at `*pos` if its key is `path` (advancing `pos`).
fn take<'l, T>(
    list: &'l [T],
    pos: &mut usize,
    key: impl Fn(&T) -> &[u8],
    path: &[u8],
) -> Option<&'l T> {
    let item = list.get(*pos).filter(|x| key(x) == path)?;
    *pos += 1;
    Some(item)
}

fn same(a: &Entry, b: &Entry) -> bool {
    a.mode == b.mode && a.id == b.id
}

struct Unpacker<'u, 'o, 'w> {
    index: &'u Index,
    wt: Option<&'u Worktree<'w>>,
    opts: &'u mut Options<'o>,
    result: Vec<Entry>,
    update: Vec<Vec<u8>>,
    remove: Vec<(Vec<u8>, bool)>,
    /// Index paths removed so far (a file there may give way to a directory).
    removed: HashSet<Vec<u8>>,
    /// Index paths already handled while clearing a directory.
    consumed: HashSet<Vec<u8>>,
    rejection: Rejection,
}

impl Unpacker<'_, '_, '_> {
    /// Records a problem; returns false.
    fn problem(&mut self, p: Problem, path: &[u8]) -> bool {
        if !self
            .rejection
            .problems
            .iter()
            .any(|(q, x)| *q == p && x == path)
        {
            self.rejection.problems.push((p, path.to_vec()));
        }
        false
    }

    fn checks_worktree(&self) -> bool {
        self.wt.is_some() && !self.opts.reset
    }

    fn one_way(
        &mut self,
        path: &[u8],
        i: Option<&Entry>,
        t: Option<&Entry>,
        unmerged: bool,
    ) -> Result<bool, Error> {
        if unmerged {
            return self.discard_conflict(path, t);
        }
        match (i, t) {
            (None, None) => {
                if unmerged && self.opts.update && self.wt.is_some() {
                    self.remove.push((path.to_vec(), false));
                }
                Ok(true)
            }
            (Some(i), None) => Ok(self.deleted(i)),
            (Some(i), Some(t)) if same(i, t) => {
                let mut update = false;
                if self.opts.reset && self.opts.update {
                    if let Some(wt) = self.wt {
                        let st = wt.checker.check(i).map_err(|e| Error::Io(e.to_string()))?;
                        update = st != Status::Unchanged && !i.mode.is_gitlink();
                    }
                }
                self.keep(i);
                if update {
                    self.update.push(path.to_vec());
                }
                Ok(true)
            }
            (i, Some(t)) => self.merged(t, i),
        }
    }

    fn two_way(
        &mut self,
        path: &[u8],
        i: Option<&Entry>,
        h: Option<&Entry>,
        m: Option<&Entry>,
        unmerged: bool,
    ) -> Result<bool, Error> {
        if unmerged {
            // Only with `reset` or `discard_unmerged`: the conflict is
            // discarded for the new tree.
            return self.discard_conflict(path, m);
        }
        let Some(i) = i else {
            return match (h, m) {
                (_, None) => Ok(true),
                (Some(h), Some(m)) if !self.opts.initial_checkout => {
                    if same(h, m) {
                        Ok(true)
                    } else {
                        Ok(self.problem(Problem::WouldOverwrite, path))
                    }
                }
                (_, Some(m)) => self.merged(m, None),
            };
        };
        let hm_same = match (h, m) {
            (None, None) => true,
            (Some(h), Some(m)) => same(h, m),
            _ => false,
        };
        if hm_same || m.is_some_and(|m| same(i, m)) {
            self.keep(i);
            return Ok(true);
        }
        if h.is_some_and(|h| same(i, h)) {
            return match m {
                None => Ok(self.deleted(i)),
                Some(m) => self.merged(m, Some(i)),
            };
        }
        Ok(self.problem(Problem::WouldOverwrite, path))
    }

    /// A conflicted path takes `t` (or goes away); its work tree file is
    /// overwritten without checks.
    fn discard_conflict(&mut self, path: &[u8], t: Option<&Entry>) -> Result<bool, Error> {
        let update = self.opts.update && self.wt.is_some();
        match t {
            Some(t) => {
                if !verify_checkout_path(t.path(), t.mode, self.opts.paths) {
                    return Err(Error::InvalidPath(t.path().to_vec()));
                }
                self.result.push(t.clone());
                if update {
                    self.update.push(path.to_vec());
                }
            }
            None => {
                if update {
                    self.remove.push((path.to_vec(), false));
                }
            }
        }
        Ok(true)
    }

    fn keep(&mut self, i: &Entry) {
        self.result.push(i.clone());
    }

    /// The index entry `i` goes away.
    fn deleted(&mut self, i: &Entry) -> bool {
        if !self.verify_uptodate(i) {
            return false;
        }
        self.removed.insert(i.path().to_vec());
        if self.opts.update && self.wt.is_some() {
            self.remove.push((i.path().to_vec(), i.mode.is_gitlink()));
        }
        true
    }

    /// The tree entry `t` replaces `old` (or is new).
    fn merged(&mut self, t: &Entry, old: Option<&Entry>) -> Result<bool, Error> {
        let ok = match old {
            Some(old) => self.verify_uptodate(old),
            None => self.verify_absent(t.path(), t.mode.is_gitlink())?,
        };
        if !ok {
            return Ok(false);
        }
        if !verify_checkout_path(t.path(), t.mode, self.opts.paths) {
            return Err(Error::InvalidPath(t.path().to_vec()));
        }
        self.result.push(t.clone());
        if self.opts.update && self.wt.is_some() {
            self.update.push(t.path().to_vec());
        }
        Ok(true)
    }

    fn verify_uptodate(&mut self, e: &Entry) -> bool {
        if !self.checks_worktree() || e.mode.is_gitlink() {
            return true;
        }
        let Some(wt) = self.wt else {
            return true;
        };
        match wt.checker.check(e) {
            // Like git's plumbing, only the cached stat data counts: a file
            // touched but unchanged is "not uptodate" until the index is
            // refreshed (porcelain refreshes before merging).
            Ok(Status::Modified | Status::TypeChanged | Status::StatOnly) => {
                self.problem(Problem::NotUptodateFile, e.path())
            }
            _ => true,
        }
    }

    fn excluded(&mut self, path: &[u8], is_dir: bool) -> bool {
        match self.opts.excludes.as_mut() {
            Some(ex) => ex
                .match_with_leading_dirs(path, is_dir)
                .is_some_and(|m| m.is_excluded()),
            None => false,
        }
    }

    /// Nothing untracked may be lost by creating `path`.
    fn verify_absent(&mut self, path: &[u8], gitlink: bool) -> Result<bool, Error> {
        if !self.checks_worktree() || !self.opts.update {
            return Ok(true);
        }
        let Some(wt) = self.wt else {
            return Ok(true);
        };
        let root = wt.root;
        for (pos, &b) in path.iter().enumerate() {
            if b != b'/' {
                continue;
            }
            let dir = &path[..pos];
            match fs::symlink_metadata(join(root, dir)) {
                Err(_) => return Ok(true),
                Ok(m) if m.is_dir() => continue,
                Ok(_) => {
                    if self.removed.contains(dir) || self.excluded(dir, false) {
                        return Ok(true);
                    }
                    return Ok(self.problem(Problem::UntrackedOverwritten, dir));
                }
            }
        }
        match fs::symlink_metadata(join(root, path)) {
            Err(_) => Ok(true),
            Ok(m) if m.is_dir() => {
                if gitlink {
                    Ok(true)
                } else {
                    Ok(self.verify_clean_subdirectory(path))
                }
            }
            Ok(_) => {
                if self.index.get(path, Stage::MERGED).is_some() && !self.removed.contains(path) {
                    // Tracked and kept: not ours to judge (D/F within the index).
                    return Ok(true);
                }
                if self.excluded(path, false) {
                    Ok(true)
                } else {
                    Ok(self.problem(Problem::UntrackedOverwritten, path))
                }
            }
        }
    }

    /// A directory at `dir` is replaced by a file: its tracked files must be
    /// up to date (they go away with it) and it may hold no untracked files
    /// that are not ignored.
    fn verify_clean_subdirectory(&mut self, dir: &[u8]) -> bool {
        let mut ok = true;
        let index = self.index;
        let range = index.dir_range(dir);
        for e in &index.entries()[range] {
            if self.consumed.contains(e.path()) {
                continue;
            }
            if e.stage() == Stage::MERGED {
                ok &= self.verify_uptodate(e);
                self.removed.insert(e.path().to_vec());
                if self.opts.update {
                    self.remove.push((e.path().to_vec(), e.mode.is_gitlink()));
                }
            }
            self.consumed.insert(e.path().to_vec());
        }
        if self.has_untracked(dir) {
            ok = self.problem(Problem::NotUptodateDir, dir);
        }
        ok
    }

    fn has_untracked(&mut self, dir: &[u8]) -> bool {
        let Some(wt) = self.wt else {
            return false;
        };
        let Ok(read) = fs::read_dir(join(wt.root, dir)) else {
            return false;
        };
        let mut names: Vec<Vec<u8>> = read
            .flatten()
            .map(|e| e.file_name().as_encoded_bytes().to_vec())
            .collect();
        names.sort();
        for name in names {
            let mut rel = dir.to_vec();
            rel.push(b'/');
            rel.extend_from_slice(&name);
            let is_dir = fs::symlink_metadata(join(wt.root, &rel)).is_ok_and(|m| m.is_dir());
            if is_dir {
                if self.excluded(&rel, true) {
                    continue;
                }
                if self.has_untracked(&rel) {
                    return true;
                }
            } else if self.index.entries_for_path(&rel).is_empty() && !self.excluded(&rel, false) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_grouping() {
        let r = Rejection {
            problems: vec![
                (Problem::NotUptodateFile, b"b".to_vec()),
                (Problem::UntrackedOverwritten, b"e".to_vec()),
                (Problem::WouldOverwrite, b"a".to_vec()),
                (Problem::NotUptodateDir, b"d".to_vec()),
                (Problem::NotUptodateFile, b"c".to_vec()),
            ],
        };
        // As `git switch` prints them (each after "error: ").
        assert_eq!(
            r.porcelain_messages(Porcelain::Checkout),
            [
                "Your local changes to the following files would be overwritten by checkout:\n\ta\nPlease commit your changes or stash them before you switch branches.",
                "Your local changes to the following files would be overwritten by checkout:\n\tb\n\tc\nPlease commit your changes or stash them before you switch branches.",
                "Updating the following directories would lose untracked files in them:\n\td\n",
                "The following untracked working tree files would be overwritten by checkout:\n\te\nPlease move or remove them before you switch branches.",
            ]
        );
        assert_eq!(
            r.plumbing_messages()[0],
            "Entry 'b' not uptodate. Cannot merge."
        );
    }
}
