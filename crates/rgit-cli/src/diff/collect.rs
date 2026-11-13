// SPDX-License-Identifier: MIT
//! Finding the changed paths: tree against tree, tree against index,
//! index against work tree and tree against work tree
//! (Documentation/git-diff-tree.adoc, git-diff-index.adoc,
//! git-diff-files.adoc). The result is a list of file pairs in path order.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::{Entry, Index};
use rgit_object::{Object, TreeEntry};
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
}

impl Pair {
    pub fn new(path: Vec<u8>, old: Option<Side>, new: Option<Side>) -> Pair {
        Pair {
            old_path: path.clone(),
            path,
            old,
            new,
            unmerged: false,
        }
    }

    fn unmerged(path: Vec<u8>, old: Option<Side>, new: Option<Side>) -> Pair {
        Pair {
            unmerged: true,
            ..Pair::new(path, old, new)
        }
    }

    /// The path shown in raw and name output.
    pub fn shown_path(&self) -> &[u8] {
        if self.old.is_some() {
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
        lit.len() > dir.len() && lit.starts_with(dir) && lit[dir.len()] == b'/'
    })
}

/// Tree against tree (`diff-tree`, `diff <commit> <commit>`).
pub struct TreeDiff<'a> {
    pub repo: &'a Repo,
    pub pathspec: &'a Pathspec,
    pub recursive: bool,
    pub show_trees: bool,
    pub pairs: Vec<Pair>,
}

impl TreeDiff<'_> {
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
        let ea = match a {
            Some(id) => read_tree(self.repo, id)?,
            None => Vec::new(),
        };
        let eb = match b {
            Some(id) => read_tree(self.repo, id)?,
            None => Vec::new(),
        };
        let (mut i, mut j) = (0, 0);
        while i < ea.len() || j < eb.len() {
            let ord = match (ea.get(i), eb.get(j)) {
                (Some(x), Some(y)) => x.cmp_tree_order(y),
                (Some(_), None) => Ordering::Less,
                _ => Ordering::Greater,
            };
            match ord {
                Ordering::Less => {
                    self.one(join(base, &ea[i].name), Some(&ea[i]), None)?;
                    i += 1;
                }
                Ordering::Greater => {
                    self.one(join(base, &eb[j].name), None, Some(&eb[j]))?;
                    j += 1;
                }
                Ordering::Equal => {
                    let (x, y) = (&ea[i], &eb[j]);
                    if x.oid != y.oid || x.mode.canonical() != y.mode.canonical() {
                        self.one(join(base, &x.name), Some(x), Some(y))?;
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
    fn walk(
        repo: &Repo,
        tree: &ObjectId,
        base: &[u8],
        ps: &Pathspec,
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
                walk(repo, &e.oid, &b, ps, out)?;
            } else if ps.is_empty() || ps.match_path(&path).is_some() {
                out.push((path, mode.bits(), e.oid));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(repo, tree, b"", pathspec, &mut out)?;
    // Tree order equals path order for files except around names that
    // sort between `name` and `name/`; sort to be safe.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// How the work tree is compared.
pub struct WorkTree<'a> {
    pub root: &'a Path,
    pub checker: &'a Checker,
    /// `git diff`: a file whose stat data changed but whose contents and
    /// mode did not is not a change (git refreshes the index first).
    pub porcelain: bool,
    pub hash: rgit_hash::HashKind,
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
        if e.flags.skip_worktree || e.flags.assume_valid {
            return Ok(None);
        }
        let Some(meta) = self.checker.lstat(e.path()).map_err(io)? else {
            return Ok(Some(None));
        };
        if e.mode.is_gitlink() {
            if !meta.is_dir() {
                return Ok(Some(None));
            }
            let head = crate::repository::submodule_head(&os_path(self.root, e.path()), self.hash);
            return Ok(match head {
                Some(h) if h != e.id => Some(Some(Side {
                    mode: GITLINK_MODE,
                    oid: h,
                    file: None,
                })),
                _ => None,
            });
        }
        let status = self.checker.check_with(e, &meta).map_err(io)?;
        Ok(match status {
            Status::Unchanged => None,
            Status::StatOnly if self.porcelain => None,
            Status::Deleted => Some(None),
            _ => Some(Some(self.file_side(e.path(), &meta, Some(e.mode.0)))),
        })
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
        // Unmerged: the `U` line, then "ours" against the work tree.
        let ours = ip.stages[1];
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
    Ok(pairs)
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
    let files = tree_files(repo, tree, ps)?;
    let paths = index_paths(index, ps);
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
        // `git diff <commit>` shows the work tree side without an id even
        // when it matches the index.
        let new = match (new, wt) {
            (Some(n), Some(wt))
                if wt.porcelain && n.file.is_none() && type_bits(n.mode) != GITLINK_MODE =>
            {
                Some(Side {
                    oid: ObjectId::null(wt.hash),
                    file: Some(os_path(wt.root, &path)),
                    ..n
                })
            }
            (n, _) => n,
        };
        pairs.push(Pair::new(path, old, new));
    }
    Ok(pairs)
}
