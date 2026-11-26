// SPDX-License-Identifier: MIT

//! One- and two-way merges of trees into the index (planning) and the work
//! tree update that carries them out.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Index, NameHash, Stage};
use rgit_worktree::ignore::Excludes;
use rgit_worktree::stat::{Checker, Status, stat_from_metadata};

use crate::Error;
use crate::cache_tree::repair_cache_tree;
use crate::checkout::{self, Filter, join, lossy};
use crate::messages as msg;
use crate::objects::{Objects, read_blob, tree_entries};
use crate::path::{PathOptions, verify_checkout_path};
use crate::submodule::{Move, Submodules};

/// The trees to merge into the index.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// `read-tree -m <base>... <head> <remote>`: the "3-Way Merge" of
    /// git-read-tree(1) with the trivial-merge rules of
    /// Documentation/technical/trivial-merge.adoc; paths that do not
    /// merge trivially stay as conflict stages 1-3.
    ThreeWay {
        /// The common ancestors (one or more).
        bases: Vec<ObjectId>,
        head: ObjectId,
        remote: ObjectId,
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
    /// A checkout into a new work tree (`git clone`): find the paths that
    /// end up in one file because the file system does not tell them apart
    /// ([`Applied::collided`]).
    pub report_collisions: bool,
    /// `--aggressive`: a three-way merge also resolves removals on one or
    /// both sides and identical additions.
    pub aggressive: bool,
    /// `core.protectNTFS`, `core.protectHFS`.
    pub paths: PathOptions,
    /// Ignored files that may be overwritten or removed (git uses the
    /// standard excludes whenever the work tree is updated).
    pub excludes: Option<&'a mut Excludes>,
    /// `--recurse-submodules`: active submodules are checked and moved
    /// along with their gitlinks (with `update`).
    pub submodules: Option<&'a dyn Submodules>,
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
    /// An active submodule cannot be moved (`--recurse-submodules`).
    SubmoduleUpdate,
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

    /// The same problems with `prefix` in front of every path (a merge
    /// run inside a submodule reports paths as the superproject sees them).
    #[must_use]
    pub fn with_prefix(&self, prefix: &[u8]) -> Rejection {
        let problems = self
            .problems
            .iter()
            .map(|(p, path)| {
                let mut full = prefix.to_vec();
                full.extend_from_slice(path);
                (*p, full)
            })
            .collect();
        Rejection { problems }
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
                    Problem::SubmoduleUpdate => msg::submodule_new_head(&path),
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
                    (Problem::SubmoduleUpdate, _) => msg::CANNOT_UPDATE_SUBMODULE,
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
    /// Submodules to move ([`Options::submodules`]).
    moves: Vec<Move>,
    report_collisions: bool,
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
    /// With [`Options::report_collisions`]: the paths that share a work
    /// tree file with another path of the checkout (they differ only in a
    /// way the file system ignores, such as case), every member of a group,
    /// in the order git lists them.
    pub collided: Vec<Vec<u8>>,
}

/// Finds the paths of a fresh checkout that end up in the same file.
#[derive(Default)]
struct Collisions {
    /// Written so far, by file identity (device and inode).
    #[cfg(unix)]
    by_file: std::collections::HashMap<(u64, u64), Vec<u8>>,
    /// Written so far, by folded name (where files have no stable identity).
    #[cfg(not(unix))]
    by_name: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    collided: HashSet<Vec<u8>>,
}

impl Collisions {
    /// Before `path` is written: a file already there was written for
    /// another path of this checkout.
    fn check(&mut self, root: &Path, path: &[u8]) {
        let Ok(meta) = fs::symlink_metadata(join(root, path)) else {
            return;
        };
        if meta.is_dir() {
            return;
        }
        self.collided.insert(path.to_vec());
        #[cfg(unix)]
        let other = {
            use std::os::unix::fs::MetadataExt as _;
            self.by_file.get(&(meta.dev(), meta.ino()))
        };
        #[cfg(not(unix))]
        let other = self.by_name.get(&path.to_ascii_lowercase());
        if let Some(other) = other {
            self.collided.insert(other.clone());
        }
    }

    /// After `path` was written.
    fn written(&mut self, path: &[u8], meta: &fs::Metadata) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            self.by_file.insert((meta.dev(), meta.ino()), path.to_vec());
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            self.by_name
                .entry(path.to_ascii_lowercase())
                .or_insert_with(|| path.to_vec());
        }
    }

    /// The collided paths of `index` as git lists them: sorted without
    /// regard to case, the members of a group in index order.
    fn into_list(self, index: &Index) -> Vec<Vec<u8>> {
        let mut list: Vec<Vec<u8>> = index
            .entries()
            .iter()
            .filter(|e| self.collided.contains(e.path()))
            .map(|e| e.path().to_vec())
            .collect();
        list.dedup();
        list.sort_by_key(|p| p.to_ascii_lowercase());
        list
    }
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
        self.apply_with_progress(odb, wt, filter, false)
    }

    /// Whether the plan moves submodules (then [`Plan::apply_submodules`]
    /// carries it out).
    pub fn moves_submodules(&self) -> bool {
        !self.moves.is_empty()
    }

    /// [`Plan::apply_with_progress`] that also moves the submodules found
    /// while planning with [`Options::submodules`]: removed ones after the
    /// files are removed and before any file is written, added and changed
    /// ones after all files are written. `force` is the merge's `reset`.
    pub fn apply_submodules(
        mut self,
        odb: &dyn Objects,
        wt: &Worktree<'_>,
        filter: &dyn Filter,
        progress: bool,
        submodules: &dyn Submodules,
        force: bool,
    ) -> Applied {
        let moves = std::mem::take(&mut self.moves);
        let mut errors = Vec::new();
        let (gone, stay): (Vec<Move>, Vec<Move>) = moves.into_iter().partition(|m| m.new.is_none());
        let remove = std::mem::take(&mut self.remove);
        for (path, gitlink) in &remove {
            if let Err(e) = checkout::remove_entry(wt.root, path, *gitlink) {
                errors.push(msg::unable_to_unlink(&lossy(path), &checkout::strerror(&e)));
            }
        }
        for m in &gone {
            match submodules.update(&m.path, m.old.as_ref(), None, force) {
                // The emptied directory goes, unless a file takes its place.
                Ok(()) if !self.update.contains(&m.path) => {
                    if let Err(e) = checkout::remove_entry(wt.root, &m.path, true) {
                        errors.push(msg::unable_to_unlink(
                            &lossy(&m.path),
                            &checkout::strerror(&e),
                        ));
                    }
                }
                Ok(()) => {}
                Err(e) => errors.push(e),
            }
        }
        let mut applied = self.apply_with_progress(odb, wt, filter, progress);
        errors.append(&mut applied.errors);
        for m in &stay {
            if let Err(e) = submodules.update(&m.path, m.old.as_ref(), m.new.as_ref(), force) {
                errors.push(e);
            }
        }
        applied.errors = errors;
        applied
    }

    /// [`Plan::apply`] with git's delayed `Updating files` meter when
    /// `progress`.
    pub fn apply_with_progress(
        self,
        odb: &dyn Objects,
        wt: &Worktree<'_>,
        filter: &dyn Filter,
        progress: bool,
    ) -> Applied {
        let Plan {
            mut index,
            remove,
            update,
            moves: _,
            report_collisions,
        } = self;
        let mut collisions = report_collisions.then(Collisions::default);
        let mut meter = progress.then(|| {
            rgit_progress::Progress::start_delayed(
                msg::PROGRESS_UPDATING_FILES,
                (remove.len() + update.len()) as u64,
                true,
            )
        });
        let mut done = 0u64;
        let mut tick = |meter: &mut Option<rgit_progress::Progress>| {
            done += 1;
            if let Some(m) = meter {
                m.set(done);
            }
        };
        let mut errors = Vec::new();
        for (path, gitlink) in &remove {
            tick(&mut meter);
            if let Err(e) = checkout::remove_entry(wt.root, path, *gitlink) {
                errors.push(msg::unable_to_unlink(&lossy(path), &checkout::strerror(&e)));
            }
        }
        let symlinks = wt.checker.options().symlinks;
        let mut delayed = 0u64;
        for path in &update {
            tick(&mut meter);
            let Ok(pos) = index.position(path, Stage::MERGED) else {
                continue;
            };
            let entry = index.entries()[pos].clone();
            if let Some(c) = &mut collisions {
                c.check(wt.root, path);
            }
            if let Some(res) = checkout::stream_entry(odb, filter, wt.root, path, &entry) {
                match res {
                    Ok(meta) => {
                        if let Some(c) = &mut collisions {
                            c.written(path, &meta);
                        }
                        if let Some(e) = index.entry_mut(pos) {
                            e.stat = stat_from_metadata(&meta);
                        }
                    }
                    Err(e) => errors.push(e.to_string()),
                }
                continue;
            }
            let data = if entry.mode.is_gitlink() {
                Vec::new()
            } else {
                match read_blob(odb, &entry.id) {
                    Ok(d) if entry.mode.is_file() => {
                        match filter.to_worktree_delayable(path, &entry.id, d) {
                            Some(d) => d,
                            None => {
                                delayed += 1;
                                continue;
                            }
                        }
                    }
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(e.to_string());
                        continue;
                    }
                }
            };
            match checkout::write_entry(wt.root, path, &entry, &data, symlinks) {
                Ok(Some(meta)) => {
                    if let Some(c) = &mut collisions {
                        c.written(path, &meta);
                    }
                    if let Some(e) = index.entry_mut(pos) {
                        e.stat = stat_from_metadata(&meta);
                    }
                }
                Ok(None) => {}
                Err(e) => errors.push(e.to_string()),
            }
        }
        // Like git, the work tree meter ends before the filters deliver
        // the delayed paths.
        if let Some(m) = meter {
            m.stop();
        }
        if delayed > 0 {
            let mut filtering = progress.then(|| {
                rgit_progress::Progress::start_delayed(msg::PROGRESS_FILTERING, delayed, true)
            });
            let mut filtered = 0u64;
            let mut filtered_bytes = 0u64;
            if let Some(m) = &mut filtering {
                m.throughput(0);
            }
            let mut ready = |path: &[u8], data: Vec<u8>| {
                if let Some(m) = &mut filtering {
                    filtered += 1;
                    filtered_bytes += data.len() as u64;
                    m.throughput(filtered_bytes);
                    m.set(filtered);
                }
                let Ok(pos) = index.position(path, Stage::MERGED) else {
                    return;
                };
                let entry = index.entries()[pos].clone();
                match checkout::write_entry(wt.root, path, &entry, &data, symlinks) {
                    Ok(Some(meta)) => {
                        if let Some(e) = index.entry_mut(pos) {
                            e.stat = stat_from_metadata(&meta);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => errors.push(e.to_string()),
                }
            };
            let missing = filter.finish_delayed(&mut ready);
            if let Some(m) = filtering {
                m.stop();
            }
            errors.extend(missing);
        }
        let collided = collisions.map_or_else(Vec::new, |c| c.into_list(&index));
        Applied {
            index,
            errors,
            collided,
        }
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
    if let Merge::ThreeWay {
        bases,
        head,
        remote,
    } = merge
    {
        return three_way(odb, index, bases, head, remote, wt, opts);
    }
    let (old, new) = match merge {
        Merge::OneWay(t) => (None, tree_entries(odb, t)?),
        Merge::TwoWay { old, new } => (Some(tree_entries(odb, old)?), tree_entries(odb, new)?),
        Merge::ThreeWay { .. } => unreachable!("handled above"),
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
        moves: Vec::new(),
        names: None,
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
    out.clear_entries();
    // Like git, a result of unpacking trees has no resolve undo records.
    out.clear_resolve_undo();
    out.set_cache_tree(None);
    let add = AddOptions {
        ok_to_replace: true,
        skip_df_check: false,
        skip_path_check: true,
    };
    // The untracked cache carries over; rebuilding entry by entry must
    // not invalidate it (the work tree tells it what changed).
    let untracked = out.take_untracked_cache();
    for e in u.result {
        out.add(e, add).map_err(|e| Error::Io(e.to_string()))?;
    }
    out.set_untracked_cache(untracked);
    let tree = repair_cache_tree(&out, odb);
    out.set_cache_tree(Some(tree));
    Ok(Plan {
        index: out,
        remove: u.remove,
        update: u.update,
        moves: u.moves,
        report_collisions: u.opts.report_collisions,
    })
}

/// See [`Merge::ThreeWay`].
fn three_way(
    odb: &dyn Objects,
    index: &Index,
    bases: &[ObjectId],
    head: &ObjectId,
    remote: &ObjectId,
    wt: Option<&Worktree<'_>>,
    opts: &mut Options<'_>,
) -> Result<Plan, Error> {
    let base_lists: Vec<Vec<Entry>> = bases
        .iter()
        .map(|b| tree_entries(odb, b))
        .collect::<Result<_, _>>()?;
    let head_list = tree_entries(odb, head)?;
    let remote_list = tree_entries(odb, remote)?;
    // Directories of the head and remote trees (for the D/F check of the
    // ALT cases).
    let dirs_of = |list: &[Entry]| -> HashSet<Vec<u8>> {
        let mut out = HashSet::new();
        for e in list {
            let p = e.path();
            for (i, &b) in p.iter().enumerate() {
                if b == b'/' {
                    out.insert(p[..i].to_vec());
                }
            }
        }
        out
    };
    let files_of =
        |list: &[Entry]| -> HashSet<Vec<u8>> { list.iter().map(|e| e.path().to_vec()).collect() };
    let (head_dirs, remote_dirs) = (dirs_of(&head_list), dirs_of(&remote_list));
    let (head_files, remote_files) = (files_of(&head_list), files_of(&remote_list));
    // `path` clashes with a directory or a leading file of `list`.
    let df = |path: &[u8], dirs: &HashSet<Vec<u8>>, files: &HashSet<Vec<u8>>| {
        dirs.contains(path)
            || path
                .iter()
                .enumerate()
                .any(|(i, &b)| b == b'/' && files.contains(&path[..i]))
    };
    let wt = if opts.index_only { None } else { wt };
    let mut u = Unpacker {
        index,
        wt,
        opts,
        result: Vec::new(),
        update: Vec::new(),
        remove: Vec::new(),
        removed: HashSet::new(),
        consumed: HashSet::new(),
        rejection: Rejection::default(),
        moves: Vec::new(),
        names: None,
    };
    let groups: Vec<&[Entry]> = index
        .entries()
        .chunk_by(|a, b| a.path() == b.path())
        .collect();
    let mut gi = 0;
    let mut bi = vec![0; base_lists.len()];
    let (mut hi, mut ri) = (0, 0);
    loop {
        let mut heads: Vec<&[u8]> = Vec::new();
        heads.extend(groups.get(gi).map(|g| g[0].path()));
        for (k, l) in base_lists.iter().enumerate() {
            heads.extend(l.get(bi[k]).map(Entry::path));
        }
        heads.extend(head_list.get(hi).map(Entry::path));
        heads.extend(remote_list.get(ri).map(Entry::path));
        let Some(path) = heads.into_iter().min().map(<[u8]>::to_vec) else {
            break;
        };
        let group = take(&groups, &mut gi, |g| g[0].path(), &path);
        let o: Vec<Option<&Entry>> = base_lists
            .iter()
            .enumerate()
            .map(|(k, l)| take(l, &mut bi[k], Entry::path, &path))
            .collect();
        let a = take(&head_list, &mut hi, Entry::path, &path);
        let b = take(&remote_list, &mut ri, Entry::path, &path);
        let i = group.and_then(|g| g.iter().find(|e| e.stage() == Stage::MERGED));
        let eq = |x: Option<&Entry>, y: Option<&Entry>| match (x, y) {
            (Some(x), Some(y)) => same(x, y),
            (None, None) => true,
            _ => false,
        };
        let any_empty = o.iter().any(Option::is_none);
        let all_empty = o.iter().all(Option::is_none);
        let first_base = o.iter().flatten().next().copied();
        let all_same = o.windows(2).all(|w| eq(w[0], w[1]));
        let any_is = |x: Option<&Entry>| x.is_some() && o.iter().any(|e| eq(*e, x));
        // The trivial result: Some(Some(e)) takes `e`, Some(None) removes
        // the path, None leaves the stages.
        let aggressive = u.opts.aggressive;
        let result: Option<Option<&Entry>> = match (a, b) {
            (None, None) if any_empty && all_empty => Some(None),
            (None, Some(b)) if any_empty && !df(&path, &head_dirs, &head_files) => Some(Some(b)),
            (Some(a), None) if any_empty && !df(&path, &remote_dirs, &remote_files) => {
                Some(Some(a))
            }
            (Some(a), Some(b)) if same(a, b) => Some(Some(a)),
            _ if all_empty => None,
            (None, None) if aggressive => Some(None),
            (None, Some(_)) if aggressive && all_same && any_is(b) => Some(None),
            (Some(_), None) if aggressive && all_same && any_is(a) => Some(None),
            (Some(_), Some(_)) if o.len() > 1 && any_is(a) && any_is(b) => None,
            (Some(a), Some(_)) if any_is(b) => Some(Some(a)),
            (Some(_), Some(b)) if any_is(a) => Some(Some(b)),
            _ => None,
        };
        // The index must match the head (or the trivial result).
        if let Some(i) = i {
            let matches_head = a.is_some_and(|a| same(i, a));
            let matches_result = matches!(result, Some(Some(r)) if same(i, r));
            if !matches_head && !matches_result {
                u.problem(Problem::WouldOverwrite, &path);
                if !u.opts.show_all_errors {
                    break;
                }
                continue;
            }
        }
        let ok = match result {
            Some(Some(r)) => match i {
                Some(i) if same(i, r) => {
                    u.keep(i);
                    true
                }
                Some(i) => u.merged(r, Some(i))?,
                None => {
                    if a.is_some_and(|a| same(a, r)) {
                        // Kept as it is in the head (no index entry).
                        u.result.push(r.clone());
                        true
                    } else {
                        u.merged(r, None)?
                    }
                }
            },
            Some(None) => match i {
                Some(i) => u.deleted(i),
                None => true,
            },
            None => {
                let ok = i.is_none_or(|i| u.verify_uptodate(i));
                for (stage, e) in [
                    (Stage::BASE, first_base),
                    (Stage::OURS, a),
                    (Stage::THEIRS, b),
                ] {
                    if let Some(e) = e {
                        u.result
                            .push(Entry::new(e.path().to_vec(), stage, e.mode, e.id));
                    }
                }
                ok
            }
        };
        if !ok && !u.opts.show_all_errors {
            break;
        }
    }
    if !u.rejection.problems.is_empty() {
        return Err(Error::Rejected(u.rejection));
    }
    let mut out = index.clone();
    out.clear_entries();
    // Like git, a result of unpacking trees has no resolve undo records.
    out.clear_resolve_undo();
    out.set_cache_tree(None);
    let add = AddOptions {
        ok_to_replace: true,
        skip_df_check: true,
        skip_path_check: true,
    };
    // The untracked cache carries over; rebuilding entry by entry must
    // not invalidate it (the work tree tells it what changed).
    let untracked = out.take_untracked_cache();
    for e in u.result {
        out.add(e, add).map_err(|e| Error::Io(e.to_string()))?;
    }
    out.set_untracked_cache(untracked);
    if !out.has_conflicts() {
        let tree = repair_cache_tree(&out, odb);
        out.set_cache_tree(Some(tree));
    }
    Ok(Plan {
        index: out,
        remove: u.remove,
        update: u.update,
        moves: u.moves,
        report_collisions: u.opts.report_collisions,
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
    /// Submodules to move.
    moves: Vec<Move>,
    /// The index paths by folded name (`core.ignoreCase`), built on first use.
    names: Option<NameHash>,
}

impl<'o> Unpacker<'_, 'o, '_> {
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

    /// With `core.ignoreCase`: the index has `path` in some case, so the
    /// file found at `path` is a tracked file under another spelling.
    fn tracked_icase(&mut self, path: &[u8]) -> bool {
        if !self.opts.paths.ignore_case {
            return false;
        }
        let index = self.index;
        self.names
            .get_or_insert_with(|| NameHash::new(index))
            .has_file(path)
    }

    /// With `core.ignoreCase`: the file found at `path` is a tracked file
    /// under another spelling, that is, the index has `path` in some case
    /// and that entry's stat data is the file's (on a file system that
    /// keeps both spellings apart, the other file is not the tracked one).
    fn same_file_icase(&mut self, path: &[u8], meta: &fs::Metadata) -> bool {
        if !self.tracked_icase(path) {
            return false;
        }
        let (Some(wt), Some(names)) = (self.wt, self.names.as_ref()) else {
            return false;
        };
        names.spellings(path).any(|spelled| {
            self.index
                .get(spelled, Stage::MERGED)
                .is_some_and(|e| matches!(wt.checker.check_with(e, meta), Ok(Status::Unchanged)))
        })
    }

    fn checks_worktree(&self) -> bool {
        self.wt.is_some() && !self.opts.reset
    }

    /// The submodule hooks when the gitlink at `path` is an active
    /// submodule whose work tree is updated.
    fn submodule(&self, path: &[u8]) -> Option<&'o dyn Submodules> {
        let s = self.opts.submodules?;
        (self.opts.update && self.wt.is_some() && s.active(path)).then_some(s)
    }

    /// Checks moving the active submodule at `path` from `old` to `new`
    /// (unless `check` is false) and records the move; false (and a
    /// problem) when it cannot move.
    fn move_submodule(
        &mut self,
        s: &dyn Submodules,
        path: &[u8],
        old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        check: bool,
    ) -> bool {
        // Nothing to check for a submodule that appears or stays.
        if check && old.is_some() && old != new && !s.check(path, old, new, self.opts.reset) {
            return self.problem(Problem::SubmoduleUpdate, path);
        }
        self.moves.push(Move {
            path: path.to_vec(),
            old: old.copied(),
            new: new.copied(),
        });
        true
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
                if self.opts.reset && i.mode.is_gitlink() {
                    if let Some(s) = self.submodule(path) {
                        // A forced checkout also resets the submodule.
                        return Ok(self.move_submodule(s, path, Some(&i.id), Some(&i.id), false));
                    }
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
        if i.mode.is_gitlink() {
            if let Some(s) = self.submodule(i.path()) {
                return self.move_submodule(s, i.path(), Some(&i.id), None, true);
            }
        }
        if self.opts.update && self.wt.is_some() {
            self.remove.push((i.path().to_vec(), i.mode.is_gitlink()));
        }
        true
    }

    /// The tree entry `t` replaces `old` (or is new).
    fn merged(&mut self, t: &Entry, old: Option<&Entry>) -> Result<bool, Error> {
        let mut ok = match old {
            Some(old) => self.verify_uptodate(old),
            None => self.verify_absent(t.path(), t.mode.is_gitlink())?,
        };
        // A submodule that moves to another commit: its staged changes
        // count as local changes (and the move is checked all the same).
        if let Some(old) = old.filter(|o| o.mode.is_gitlink() && t.mode.is_gitlink()) {
            if self.checks_worktree() {
                if let Some(s) = self.submodule(old.path()) {
                    if !s.index_clean(old.path()) {
                        ok = self.problem(Problem::NotUptodateFile, old.path());
                        let _ = self.move_submodule(s, t.path(), Some(&old.id), Some(&t.id), true);
                    }
                }
            }
        }
        if !ok {
            return Ok(false);
        }
        if !verify_checkout_path(t.path(), t.mode, self.opts.paths) {
            return Err(Error::InvalidPath(t.path().to_vec()));
        }
        let old_sub = old.filter(|o| o.mode.is_gitlink()).map(|o| o.id);
        let new_sub = t.mode.is_gitlink().then_some(t.id);
        if old_sub.is_some() || new_sub.is_some() {
            if let Some(s) = self.submodule(t.path()) {
                // Like git, a submodule replaced by a file is not checked.
                let check = new_sub.is_some();
                if !self.move_submodule(s, t.path(), old_sub.as_ref(), new_sub.as_ref(), check) {
                    return Ok(false);
                }
            }
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
        // An assume-unchanged entry that has to change is checked for real:
        // its local changes must not be lost either.
        let status = if e.flags.assume_valid && !e.flags.skip_worktree {
            match wt.checker.lstat(e.path()) {
                Ok(Some(meta)) => wt.checker.check_with(e, &meta),
                Ok(None) => Ok(Status::Deleted),
                Err(err) => Err(err),
            }
        } else {
            wt.checker.check(e)
        };
        match status {
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
                Ok(m) => {
                    if self.removed.contains(dir)
                        || self.same_file_icase(dir, &m)
                        || self.excluded(dir, false)
                    {
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
            Ok(m) => {
                if self.index.get(path, Stage::MERGED).is_some() && !self.removed.contains(path) {
                    // Tracked and kept: not ours to judge (D/F within the index).
                    return Ok(true);
                }
                if self.same_file_icase(path, &m) {
                    // The same file under another spelling (a rename that
                    // changes only the case, or two colliding paths).
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
        let exact = &index.entries()[range.clone()];
        // With core.ignoreCase the directory also holds the entries that
        // spell it differently; they are not contiguous in the index.
        let ignore_case = self.opts.paths.ignore_case;
        let folded = index.entries().iter().enumerate().filter(|(i, e)| {
            ignore_case
                && !range.contains(i)
                && e.path().len() > dir.len()
                && e.path()[dir.len()] == b'/'
                && e.path()[..dir.len()].eq_ignore_ascii_case(dir)
        });
        let below: Vec<&Entry> = exact.iter().chain(folded.map(|(_, e)| e)).collect();
        for e in below {
            if self.consumed.contains(e.path()) {
                continue;
            }
            if e.stage() == Stage::MERGED {
                ok &= self.verify_uptodate(e);
                self.removed.insert(e.path().to_vec());
                if e.mode.is_gitlink() {
                    if let Some(s) = self.submodule(e.path()) {
                        ok &= self.move_submodule(s, e.path(), Some(&e.id), None, true);
                        self.consumed.insert(e.path().to_vec());
                        continue;
                    }
                }
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
            .map(|e| rgit_worktree::precompose::file_name(&e.file_name()))
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
            } else if self.index.entries_for_path(&rel).is_empty()
                && !self.tracked_icase(&rel)
                && !self.excluded(&rel, false)
            {
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

    #[test]
    fn submodule_problems() {
        let r = Rejection {
            problems: vec![
                (Problem::SubmoduleUpdate, b"sub".to_vec()),
                (Problem::NotUptodateFile, b"sub".to_vec()),
            ],
        };
        // As `git checkout --recurse-submodules` prints them.
        assert_eq!(
            r.porcelain_messages(Porcelain::Checkout),
            [
                "Your local changes to the following files would be overwritten by checkout:\n\tsub\nPlease commit your changes or stash them before you switch branches.",
                "Cannot update submodule:\n\tsub\n",
            ]
        );
        // Inside a submodule, paths as the superproject sees them.
        assert_eq!(
            r.with_prefix(b"top/").plumbing_messages(),
            [
                "Submodule 'top/sub' cannot checkout new HEAD.",
                "Entry 'top/sub' not uptodate. Cannot merge.",
            ]
        );
    }
}
