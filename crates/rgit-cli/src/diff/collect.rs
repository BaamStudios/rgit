// SPDX-License-Identifier: MIT
//! Finding the changed paths: tree against tree, tree against index,
//! index against work tree and tree against work tree
//! (Documentation/git-diff-tree.adoc, git-diff-index.adoc,
//! git-diff-files.adoc). The result is a list of file pairs in path order.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::{Entry, Index};
use rgit_object::{EntryMode, Object, TreeEntry, TreeEntryRef, TreeIter, entry_cmp};
use rgit_pathspec::{DirInterest, Pathspec};
use rgit_worktree::stat::{Checker, Status, mode_from_metadata};

use crate::Die;
use crate::messages::diff as msg;
use crate::repository::Repo;

/// One side of a file pair.
#[derive(Debug, Clone)]
pub struct Side {
    pub mode: u32,
    /// The object id; the null id when the contents are in `file` and have
    /// not been hashed.
    pub oid: ObjectId,
    /// A file (work tree or `--no-index`) holding the contents.
    pub file: Option<PathBuf>,
}

impl Side {
    fn object(mode: u32, oid: ObjectId) -> Side {
        Side {
            mode,
            oid,
            file: None,
        }
    }
}

/// A changed path.
#[derive(Debug, Clone)]
pub struct Pair {
    /// The path of the old side.
    pub old_path: Vec<u8>,
    /// The path of the new side (the same as `old_path` except for
    /// `--no-index`).
    pub path: Vec<u8>,
    pub old: Option<Side>,
    pub new: Option<Side>,
    /// An unmerged path (status `U`).
    pub unmerged: bool,
    /// `R` or `C` after rename detection.
    pub renamed: Option<u8>,
    /// A submodule in the work tree with modified (`DIRTY_MODIFIED`) or
    /// untracked (`DIRTY_UNTRACKED`) content.
    pub dirty_submodule: u8,
    /// Similarity of a rename or copy, dissimilarity of a complete
    /// rewrite (`rgit_diff::diffcore::MAX_SCORE` units); 0 for none.
    pub score: u32,
}

impl Pair {
    pub fn new(path: Vec<u8>, old: Option<Side>, new: Option<Side>) -> Pair {
        Pair {
            old_path: path.clone(),
            path,
            old,
            new,
            unmerged: false,
            renamed: None,
            score: 0,
            dirty_submodule: 0,
        }
    }

    /// A rename or copy.
    pub fn is_rename(&self) -> bool {
        self.renamed.is_some()
    }

    /// A complete rewrite (modification with a dissimilarity).
    pub fn is_rewrite(&self) -> bool {
        self.renamed.is_none() && self.score > 0
    }

    /// The score in percent, rounded down.
    pub fn percent(&self) -> u32 {
        self.score * 100 / rgit_diff::diffcore::MAX_SCORE
    }

    fn unmerged(path: Vec<u8>, old: Option<Side>, new: Option<Side>) -> Pair {
        Pair {
            unmerged: true,
            ..Pair::new(path, old, new)
        }
    }

    /// The path shown in raw and name output.
    pub fn shown_path(&self) -> &[u8] {
        if self.old.is_some() && !self.is_rename() {
            &self.old_path
        } else {
            &self.path
        }
    }

    /// The status letter of the raw format.
    pub fn status(&self) -> u8 {
        if self.unmerged {
            return b'U';
        }
        if let Some(c) = self.renamed {
            return c;
        }
        match (&self.old, &self.new) {
            (None, _) => b'A',
            (_, None) => b'D',
            (Some(o), Some(n)) if type_bits(o.mode) != type_bits(n.mode) => b'T',
            _ => b'M',
        }
    }
}

pub fn type_bits(mode: u32) -> u32 {
    mode & 0o170000
}

pub const TREE_MODE: u32 = 0o040000;
pub const GITLINK_MODE: u32 = 0o160000;

/// [`Pair::dirty_submodule`]: tracked files of the submodule changed.
pub const DIRTY_MODIFIED: u8 = 1;
/// [`Pair::dirty_submodule`]: the submodule has untracked files.
pub const DIRTY_UNTRACKED: u8 = 2;

/// A path below `root`.
pub fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(rel).as_ref())
    }
}

fn read_tree(repo: &Repo, id: &ObjectId) -> Result<Vec<TreeEntry>, Die> {
    match repo.read_object(id) {
        Some(Object::Tree(t)) => Ok(t.entries),
        None if rgit_hash::hash_object(id.kind(), "tree", b"").ok() == Some(*id) => Ok(Vec::new()),
        _ => Err(Die::Fatal(msg::could_not_read(&id.to_hex()))),
    }
}

/// The content of tree `id` (for [`TreeEntryRef`]s borrowing from it).
fn read_tree_data(repo: &Repo, id: &ObjectId) -> Result<std::rc::Rc<[u8]>, Die> {
    match repo.read_tree_data(id) {
        Some(data) => Ok(data),
        None if rgit_hash::hash_object(id.kind(), "tree", b"").ok() == Some(*id) => {
            Ok(Vec::new().into())
        }
        None => Err(Die::Fatal(msg::could_not_read(&id.to_hex()))),
    }
}

/// The length of the common prefix of `a` and `b`.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    const STEP: usize = 64;
    let max = a.len().min(b.len());
    let mut n = 0;
    while n + STEP <= max && a[n..n + STEP] == b[n..n + STEP] {
        n += STEP;
    }
    while n < max && a[n] == b[n] {
        n += 1;
    }
    n
}

/// The length of the common suffix of `a` and `b`.
fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    const STEP: usize = 64;
    let max = a.len().min(b.len());
    let mut n = 0;
    while n + STEP <= max
        && a[a.len() - n - STEP..a.len() - n] == b[b.len() - n - STEP..b.len() - n]
    {
        n += STEP;
    }
    while n < max && a[a.len() - 1 - n] == b[b.len() - 1 - n] {
        n += 1;
    }
    n
}

/// Checks every entry of the tree `id` with the content `data`.
fn check_tree(id: &ObjectId, data: &[u8]) -> Result<(), Die> {
    let mut iter = TreeIter::new(id.kind(), data);
    while let Some(r) = iter.skip_entry() {
        r.map_err(|_| Die::Fatal(msg::could_not_read(&id.to_hex())))?;
    }
    Ok(())
}

/// The entries in which the trees `a` and `b` (contents `da` and `db`)
/// may differ. Runs of entries that are byte for byte the same at the
/// start and at the end of both trees are left out without being
/// compared. Every entry of `a` is checked and the left-out entries of
/// `b` are the same bytes, so a malformed tree is still found.
fn differing_entries<'d>(
    a: Option<&ObjectId>,
    da: &'d [u8],
    b: Option<&ObjectId>,
    db: &'d [u8],
) -> Result<(Vec<TreeEntryRef<'d>>, Vec<TreeEntryRef<'d>>), Die> {
    let bad = |id: &ObjectId| Die::Fatal(msg::could_not_read(&id.to_hex()));
    let pre = if b.is_some() {
        common_prefix(da, db)
    } else {
        0
    };
    // `a` is checked; entries that end inside the common prefix are the
    // same in `b` (both trees start with the same bytes up to there).
    // For the others the length of the content after them is kept.
    let mut skip = 0;
    let mut a_rests: Vec<usize> = Vec::new();
    if let Some(a) = a {
        let mut iter = TreeIter::new(a.kind(), da);
        while let Some(r) = iter.skip_entry() {
            r.map_err(|_| bad(a))?;
            let rest = iter.rest().len();
            if a_rests.is_empty() && da.len() - rest <= pre {
                skip = da.len() - rest;
            } else {
                a_rests.push(rest);
            }
        }
    }
    let mut a_end = a_rests.len();
    let mut eb = Vec::new();
    if let Some(b) = b {
        let suf = common_suffix(&da[skip..], &db[skip..]);
        let a_rest = da.len() - skip;
        let mut iter = TreeIter::new(b.kind(), &db[skip..]);
        loop {
            let rest = iter.rest().len();
            if rest <= suf {
                // The rest of `b` is the same as the rest of `a` from
                // where as much content is left (`a_rests` decrease).
                if rest == a_rest {
                    a_end = 0;
                    break;
                }
                if let Ok(n) = a_rests.binary_search_by(|r| rest.cmp(r)) {
                    a_end = n + 1;
                    break;
                }
            }
            match iter.next() {
                Some(e) => eb.push(e.map_err(|_| bad(b))?),
                None => break,
            }
        }
    }
    let ea = match (a, a_end) {
        (Some(a), n) if n > 0 => {
            let end = da.len() - a_rests[n - 1];
            TreeIter::new(a.kind(), &da[skip..end])
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| bad(a))?
        }
        _ => Vec::new(),
    };
    Ok((ea, eb))
}

fn join(base: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = base.to_vec();
    p.extend_from_slice(name);
    p
}

/// Whether `dir` is a leading directory of the literal part of a positive
/// pathspec item (then the non-recursive tree diff shows it).
fn leads_to(ps: &Pathspec, dir: &[u8]) -> bool {
    ps.items().iter().filter(|i| !i.is_exclude()).any(|i| {
        let lit = &i.pattern()[..i.nowildcard_len()];
        // `:(icase)` compares case-insensitively (t6131).
        lit.len() > dir.len() && i.literal_eq(dir, dir.len()) && lit[dir.len()] == b'/'
    })
}

/// Tree against tree (`diff-tree`, `diff <commit> <commit>`).
pub struct TreeDiff<'a> {
    pub repo: &'a Repo,
    pub pathspec: &'a Pathspec,
    pub recursive: bool,
    pub show_trees: bool,
    /// `--max-depth=<n>`: directories this deep below a pathspec item (or
    /// the top) are shown as trees instead of being entered.
    pub max_depth: Option<usize>,
    pub pairs: Vec<Pair>,
}

impl TreeDiff<'_> {
    /// The depth of the directory `path` for `--max-depth`: below the
    /// closest pathspec item that contains it (top-level entries count 0
    /// without pathspecs); `None` for a directory leading to an item.
    fn depth(&self, path: &[u8]) -> Option<usize> {
        let components = |p: &[u8]| p.iter().filter(|&&c| c == b'/').count();
        if self.pathspec.is_empty() {
            return Some(components(path));
        }
        self.pathspec
            .items()
            .iter()
            .filter(|i| !i.is_exclude())
            .filter_map(|i| {
                let lit = i.pattern().strip_suffix(b"/").unwrap_or(i.pattern());
                if lit.is_empty() {
                    Some(components(path))
                } else if path == lit {
                    Some(0)
                } else if path.starts_with(lit) && path.get(lit.len()) == Some(&b'/') {
                    Some(components(&path[lit.len() + 1..]) + 1)
                } else {
                    None
                }
            })
            .min()
    }

    fn side(e: &TreeEntry) -> Side {
        Side::object(e.mode.canonical().bits(), e.oid)
    }

    fn one(
        &mut self,
        path: Vec<u8>,
        old: Option<&TreeEntry>,
        new: Option<&TreeEntry>,
    ) -> Result<(), Die> {
        let ps = self.pathspec;
        let is_tree = old.or(new).is_some_and(|e| e.mode.is_tree());
        if is_tree {
            let mut descend = true;
            if !ps.is_empty() {
                // Whether the tree itself is shown ignores exclude items.
                let positive = |i: &rgit_pathspec::PathspecItem| {
                    !i.is_exclude()
                        && (i.match_path(&path).is_some() || (self.recursive && i.has_wildcard()))
                };
                let shown = ps.items().iter().any(positive) || leads_to(ps, &path);
                if !shown {
                    return Ok(());
                }
                descend = ps.dir_interest(&path) != DirInterest::None;
            }
            if self.recursive && !descend {
                if self.show_trees {
                    self.pairs
                        .push(Pair::new(path, old.map(Self::side), new.map(Self::side)));
                }
                return Ok(());
            }
            if let (true, Some(max), Some(d)) = (self.recursive, self.max_depth, self.depth(&path))
            {
                if d >= max {
                    self.pairs
                        .push(Pair::new(path, old.map(Self::side), new.map(Self::side)));
                    return Ok(());
                }
            }
            if self.recursive {
                if self.show_trees {
                    self.pairs.push(Pair::new(
                        path.clone(),
                        old.map(Self::side),
                        new.map(Self::side),
                    ));
                }
                let mut base = path;
                base.push(b'/');
                return self.walk(old.map(|e| &e.oid), new.map(|e| &e.oid), &base);
            }
        } else if !ps.is_empty() && ps.match_path(&path).is_none() {
            return Ok(());
        }
        self.pairs
            .push(Pair::new(path, old.map(Self::side), new.map(Self::side)));
        Ok(())
    }

    pub fn walk(
        &mut self,
        a: Option<&ObjectId>,
        b: Option<&ObjectId>,
        base: &[u8],
    ) -> Result<(), Die> {
        // The entries borrow from the tree contents; only changed ones
        // are copied.
        let da = match a {
            Some(id) => read_tree_data(self.repo, id)?,
            None => Vec::new().into(),
        };
        let db = match b {
            Some(id) => match read_tree_data(self.repo, id) {
                Ok(d) => d,
                Err(e) => {
                    // A malformed `a` is reported first.
                    if let Some(a) = a {
                        check_tree(a, &da)?;
                    }
                    return Err(e);
                }
            },
            None => Vec::new().into(),
        };
        let (ea, eb) = differing_entries(a, &da, b, &db)?;
        let canonical = |e: &TreeEntryRef<'_>| EntryMode::from_bits(e.mode_bits).canonical();
        let (mut i, mut j) = (0, 0);
        while i < ea.len() || j < eb.len() {
            let ord = match (ea.get(i), eb.get(j)) {
                (Some(x), Some(y)) => entry_cmp(x.name, x.is_tree(), y.name, y.is_tree()),
                (Some(_), None) => Ordering::Less,
                _ => Ordering::Greater,
            };
            match ord {
                Ordering::Less => {
                    self.one(join(base, ea[i].name), Some(&ea[i].to_owned()), None)?;
                    i += 1;
                }
                Ordering::Greater => {
                    self.one(join(base, eb[j].name), None, Some(&eb[j].to_owned()))?;
                    j += 1;
                }
                Ordering::Equal => {
                    let (x, y) = (&ea[i], &eb[j]);
                    if x.oid != y.oid || canonical(x) != canonical(y) {
                        self.one(join(base, x.name), Some(&x.to_owned()), Some(&y.to_owned()))?;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        Ok(())
    }
}

/// Every file of a tree (recursively), in path order, limited to the
/// pathspec: `(path, mode, id)`.
pub fn tree_files(
    repo: &Repo,
    tree: &ObjectId,
    pathspec: &Pathspec,
) -> Result<Vec<(Vec<u8>, u32, ObjectId)>, Die> {
    tree_files_skipping(repo, tree, pathspec, &|_| false)
}

/// [`tree_files`] leaving out the directories (paths with trailing `/`)
/// for which `skip` is true.
fn tree_files_skipping(
    repo: &Repo,
    tree: &ObjectId,
    pathspec: &Pathspec,
    skip: &dyn Fn(&[u8]) -> bool,
) -> Result<Vec<(Vec<u8>, u32, ObjectId)>, Die> {
    fn walk(
        repo: &Repo,
        tree: &ObjectId,
        base: &[u8],
        ps: &Pathspec,
        skip: &dyn Fn(&[u8]) -> bool,
        out: &mut Vec<(Vec<u8>, u32, ObjectId)>,
    ) -> Result<(), Die> {
        for e in read_tree(repo, tree)? {
            let path = join(base, &e.name);
            let mode = e.mode.canonical();
            if mode.is_tree() {
                if ps.dir_interest(&path) == DirInterest::None {
                    continue;
                }
                let mut b = path;
                b.push(b'/');
                if skip(&b) {
                    continue;
                }
                walk(repo, &e.oid, &b, ps, skip, out)?;
            } else if ps.is_empty() || ps.match_path(&path).is_some() {
                out.push((path, mode.bits(), e.oid));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(repo, tree, b"", pathspec, skip, &mut out)?;
    // Tree order equals path order for files except around names that
    // sort between `name` and `name/`; sort to be safe.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// `--find-copies-harder`: add the files of the old side that did not
/// change as unmodified pairs (copy sources), in path order.
pub fn with_unmodified(
    pairs: Vec<Pair>,
    files: impl IntoIterator<Item = (Vec<u8>, u32, ObjectId)>,
) -> Vec<Pair> {
    let present: std::collections::HashSet<Vec<u8>> = pairs
        .iter()
        .flat_map(|p| [p.old_path.clone(), p.path.clone()])
        .collect();
    let mut all = pairs;
    for (path, mode, oid) in files {
        if !present.contains(&path) {
            let side = Side::object(mode, oid);
            all.push(Pair::new(path, Some(side.clone()), Some(side)));
        }
    }
    all.sort_by(|a, b| a.path.cmp(&b.path));
    all
}

/// The stage-0 entries of the index as `(path, mode, id)`.
pub fn index_files(index: &Index, ps: &Pathspec) -> Vec<(Vec<u8>, u32, ObjectId)> {
    index
        .entries()
        .iter()
        .filter(|e| e.stage().get() == 0 && (ps.is_empty() || ps.match_path(e.path()).is_some()))
        .map(|e| (e.path().to_vec(), e.mode.0, e.id))
        .collect()
}

/// How the work tree is compared.
pub struct WorkTree<'a> {
    pub root: &'a Path,
    pub checker: &'a Checker,
    pub hash: rgit_hash::HashKind,
    /// The `ignore` settings of submodules (none: count everything but
    /// untracked content).
    pub submodules: Option<&'a crate::submodules::IgnoreRules>,
    /// Dirty submodules found so far (path, `DIRTY_*` flags).
    pub dirty: std::cell::RefCell<Vec<(Vec<u8>, u8)>>,
}

impl WorkTree<'_> {
    fn file_side(&self, path: &[u8], meta: &std::fs::Metadata, old_mode: Option<u32>) -> Side {
        let old = old_mode.map(rgit_index::Mode);
        let mode = mode_from_metadata(meta, self.checker.options(), old)
            .map_or_else(|| old_mode.unwrap_or(0o100644), |m| m.0);
        Side {
            mode,
            oid: ObjectId::null(self.hash),
            file: Some(os_path(self.root, path)),
        }
    }

    /// The work tree side of a stage-0 entry: `None` when it matches the
    /// entry, `Some(None)` when the file is gone, else the file.
    fn changed(&self, e: &Entry) -> Result<Option<Option<Side>>, Die> {
        let io = |err: std::io::Error| Die::Fatal(err.to_string());
        if e.flags.skip_worktree || e.flags.assume_valid || self.checker.fsmonitor_valid(e) {
            return Ok(None);
        }
        let Some(meta) = self.checker.lstat(e.path()).map_err(io)? else {
            return Ok(Some(None));
        };
        if e.mode.is_gitlink() {
            if !meta.is_dir() {
                // The submodule was replaced by a file.
                return Ok(Some(Some(self.file_side(e.path(), &meta, None))));
            }
            let dir = os_path(self.root, e.path());
            let ignore = self
                .submodules
                .map_or(rgit_submodule::Ignore::Untracked, |r| r.for_path(e.path()));
            let st = crate::submodules::worktree_state(&dir, &e.id, self.hash, ignore, true);
            if !st.changed() {
                return Ok(None);
            }
            if st.dirty() {
                let flags = if st.modified { DIRTY_MODIFIED } else { 0 }
                    | if st.untracked { DIRTY_UNTRACKED } else { 0 };
                self.dirty.borrow_mut().push((e.path().to_vec(), flags));
            }
            // The work tree side is not hashed: the null id when the
            // submodule moved, else the recorded commit (only dirty).
            return Ok(Some(Some(Side {
                mode: GITLINK_MODE,
                oid: if st.new_commits {
                    ObjectId::null(self.hash)
                } else {
                    e.id
                },
                file: Some(dir),
            })));
        }
        if meta.is_dir() && !e.mode.is_symlink() {
            // A file replaced by a repository: a submodule now.
            let dir = os_path(self.root, e.path());
            if crate::repository::submodule_head(&dir, self.hash).is_some() {
                return Ok(Some(Some(Side {
                    mode: GITLINK_MODE,
                    oid: ObjectId::null(self.hash),
                    file: Some(dir),
                })));
            }
        }
        let status = self.checker.check_with(e, &meta).map_err(io)?;
        // A file whose stat data changed is not known to match the entry
        // (git does not refresh the index): it is shown without an id and
        // read again for the output, where `git diff` drops it as
        // unchanged (and prints conversion warnings).
        Ok(match status {
            Status::Unchanged => None,
            Status::Deleted => Some(None),
            _ => Some(Some(self.file_side(e.path(), &meta, Some(e.mode.0)))),
        })
    }

    /// Mark the pairs of dirty submodules found by [`Self::changed`].
    fn mark_dirty(&self, pairs: &mut [Pair]) {
        let dirty = self.dirty.borrow();
        if dirty.is_empty() {
            return;
        }
        for p in pairs {
            if let Some((_, f)) = dirty.iter().find(|(path, _)| *path == p.path) {
                if p.new.as_ref().is_some_and(|s| s.file.is_some()) {
                    p.dirty_submodule = *f;
                }
            }
        }
    }

    /// The work tree file of an unmerged path, if any.
    fn unmerged_file(&self, path: &[u8], mode: Option<u32>) -> Result<Option<Side>, Die> {
        let meta = self
            .checker
            .lstat(path)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(meta
            .filter(|m| !m.is_dir())
            .map(|m| self.file_side(path, &m, mode)))
    }
}

/// The index entries grouped by path: stage 0 entry or conflict stages.
struct IndexPath<'a> {
    path: &'a [u8],
    stage0: Option<&'a Entry>,
    /// Stages 1..=3.
    stages: [Option<&'a Entry>; 3],
}

fn index_paths<'a>(index: &'a Index, ps: &Pathspec) -> Vec<IndexPath<'a>> {
    let mut out: Vec<IndexPath<'a>> = Vec::new();
    for e in index.entries() {
        if !ps.is_empty() && ps.match_path(e.path()).is_none() {
            continue;
        }
        if out.last().is_none_or(|l| l.path != e.path()) {
            out.push(IndexPath {
                path: e.path(),
                stage0: None,
                stages: [None; 3],
            });
        }
        let last = out.last_mut().expect("pushed");
        match e.stage().get() {
            0 => last.stage0 = Some(e),
            s => last.stages[usize::from(s) - 1] = Some(e),
        }
    }
    out
}

fn entry_side(e: &Entry) -> Side {
    Side::object(e.mode.0, e.id)
}

/// Index against work tree (`diff-files`, `diff`).
pub fn files(
    index: &Index,
    ps: &Pathspec,
    wt: &WorkTree<'_>,
    skip_deleted: bool,
) -> Result<Vec<Pair>, Die> {
    files_stage(index, ps, wt, skip_deleted, 2)
}

/// [`files`] comparing unmerged paths' conflict `stage` (1 base, 2 ours,
/// 3 theirs; 0 none) against the work tree (`diff-files -0`..`-3`).
pub fn files_stage(
    index: &Index,
    ps: &Pathspec,
    wt: &WorkTree<'_>,
    skip_deleted: bool,
    stage: usize,
) -> Result<Vec<Pair>, Die> {
    let mut pairs = Vec::new();
    for ip in index_paths(index, ps) {
        let path = ip.path.to_vec();
        if let Some(e) = ip.stage0 {
            if e.flags.intent_to_add {
                if let Some(side) = wt.unmerged_file(&path, Some(e.mode.0))? {
                    pairs.push(Pair::new(path, None, Some(side)));
                } else if !skip_deleted {
                    pairs.push(Pair::new(path, Some(entry_side(e)), None));
                }
                continue;
            }
            match wt.changed(e)? {
                None => {}
                Some(None) if skip_deleted => {}
                Some(new) => pairs.push(Pair::new(path, Some(entry_side(e)), new)),
            }
            continue;
        }
        // Unmerged: the `U` line, then the chosen stage ("ours" by default)
        // against the work tree.
        let ours = stage.checked_sub(1).and_then(|s| ip.stages[s]);
        let file = wt.unmerged_file(&path, ours.map(|e| e.mode.0))?;
        let shown = file.as_ref().map(|s| Side {
            mode: s.mode,
            oid: ObjectId::null(wt.hash),
            file: None,
        });
        pairs.push(Pair::unmerged(path.clone(), None, shown));
        if let (Some(o), Some(f)) = (ours, file) {
            pairs.push(Pair::new(path, Some(entry_side(o)), Some(f)));
        }
    }
    wt.mark_dirty(&mut pairs);
    Ok(pairs)
}

/// An unmerged path: its index stages 1 to 3 and its work tree file.
pub struct Unmerged {
    pub path: Vec<u8>,
    pub stages: [Option<Side>; 3],
    pub file: Option<Side>,
}

/// The unmerged paths of the index (for combined diffs and `-1`..`-3`).
pub fn unmerged(index: &Index, ps: &Pathspec, wt: &WorkTree<'_>) -> Result<Vec<Unmerged>, Die> {
    let mut out = Vec::new();
    for ip in index_paths(index, ps) {
        if ip.stage0.is_some() {
            continue;
        }
        let mode = ip.stages[1]
            .or(ip.stages[2])
            .or(ip.stages[0])
            .map(|e| e.mode.0);
        out.push(Unmerged {
            path: ip.path.to_vec(),
            stages: ip.stages.map(|e| e.map(entry_side)),
            file: wt.unmerged_file(ip.path, mode)?,
        });
    }
    Ok(out)
}

/// Tree against index (`diff-index --cached`, `diff --cached`), or
/// against the work tree when `wt` is given (`diff-index`, `diff <commit>`).
pub fn index(
    repo: &Repo,
    tree: &ObjectId,
    index: &Index,
    ps: &Pathspec,
    wt: Option<&WorkTree<'_>>,
    ita_invisible: bool,
) -> Result<Vec<Pair>, Die> {
    // Sparse directory entries of an index read with `read_index_sparse`
    // hold what `tree` has there (the caller made sure): both sides skip
    // them.
    let sparse_dir = |dir: &[u8]| {
        index
            .get(dir, rgit_index::Stage::MERGED)
            .is_some_and(|e| e.mode.is_dir())
    };
    let files = if index.is_sparse() {
        tree_files_skipping(repo, tree, ps, &sparse_dir)?
    } else {
        tree_files(repo, tree, ps)?
    };
    // A tree with duplicate entries cannot match the index (observed:
    // t4058 "git diff HEAD does not segfault").
    if wt.is_some() && files.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(Die::Error(
            crate::messages::diff::CORRUPTED_CACHE_TREE.into(),
            128,
        ));
    }
    let mut paths = index_paths(index, ps);
    paths.retain(|p| p.stage0.is_none_or(|e| !e.mode.is_dir()));
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < files.len() || j < paths.len() {
        let ord = match (files.get(i), paths.get(j)) {
            (Some(f), Some(p)) => f.0.as_slice().cmp(p.path),
            (Some(_), None) => Ordering::Less,
            _ => Ordering::Greater,
        };
        let (tree_side, ip) = match ord {
            Ordering::Less => {
                i += 1;
                (Some(&files[i - 1]), None)
            }
            Ordering::Greater => {
                j += 1;
                (None, Some(&paths[j - 1]))
            }
            Ordering::Equal => {
                i += 1;
                j += 1;
                (Some(&files[i - 1]), Some(&paths[j - 1]))
            }
        };
        let path = tree_side.map_or_else(|| ip.expect("one side").path.to_vec(), |f| f.0.clone());
        let old = tree_side.map(|f| Side::object(f.1, f.2));
        let Some(ip) = ip else {
            pairs.push(Pair::new(path, old, None));
            continue;
        };
        let Some(e) = ip.stage0 else {
            // Unmerged.
            match wt {
                None => {
                    pairs.push(Pair::unmerged(path, old, None));
                }
                Some(wt) => {
                    if let Some(f) = wt.unmerged_file(&path, old.as_ref().map(|s| s.mode))? {
                        pairs.push(Pair::new(path, old, Some(f)));
                    } else if old.is_some() {
                        pairs.push(Pair::new(path, old, None));
                    }
                }
            }
            continue;
        };
        if e.flags.intent_to_add && (ita_invisible || wt.is_some()) {
            match wt {
                None => {
                    if old.is_some() {
                        pairs.push(Pair::new(path, old, None));
                    }
                }
                Some(wt) => match wt.unmerged_file(&path, Some(e.mode.0))? {
                    Some(f) => pairs.push(Pair::new(path, old, Some(f))),
                    None => {
                        if old.is_some() {
                            pairs.push(Pair::new(path, old, None));
                        }
                    }
                },
            }
            continue;
        }
        let new = match wt {
            None => Some(entry_side(e)),
            Some(wt) => match wt.changed(e)? {
                None => Some(entry_side(e)),
                Some(None) => None,
                Some(Some(f)) => Some(f),
            },
        };
        if new.is_none() && old.is_none() {
            continue;
        }
        if let (Some(o), Some(n)) = (&old, &new) {
            if n.file.is_none() && o.oid == n.oid && o.mode == n.mode {
                continue;
            }
        }
        pairs.push(Pair::new(path, old, new));
    }
    if let Some(wt) = wt {
        wt.mark_dirty(&mut pairs);
    }
    Ok(pairs)
}
