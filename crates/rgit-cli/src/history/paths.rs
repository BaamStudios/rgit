// SPDX-License-Identifier: MIT
//! Path limiting for `rev-list` and `log` (the "History Simplification"
//! section of Documentation/rev-list-options.adoc): whether a commit is
//! TREESAME to a parent, comparing only the paths of the pathspec, and
//! `--simplify-by-decoration`.

use std::collections::HashMap;

use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, TreeIter};
use rgit_pathspec::{DirInterest, ParseOptions, Pathspec};
use rgit_revwalk::revision::ObjectLookup;

use super::Repo;
use super::decorate::Decorations;
use crate::{Context, Die};

/// What decides TREESAME.
pub struct PathFilter {
    /// Empty: every path (only with `--simplify-by-decoration`).
    pub pathspec: Pathspec,
    /// `--simplify-by-decoration`: decorated commits are never TREESAME.
    pub decorations: Option<Decorations>,
    trees: HashMap<ObjectId, Option<ObjectId>>,
}

/// One tree entry.
struct Item {
    name: Vec<u8>,
    mode: u32,
    id: ObjectId,
}

impl Item {
    fn is_tree(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
}

/// Parse the pathspecs `args` relative to the current directory.
pub fn parse_pathspec(ctx: &Context, repo: &Repo<'_>, args: &[String]) -> Result<Pathspec, Die> {
    let prefix = repo
        .setup
        .prefix
        .as_ref()
        .map(|p| {
            let mut p = p.as_encoded_bytes().to_vec();
            if !p.is_empty() && !p.ends_with(b"/") {
                p.push(b'/');
            }
            p
        })
        .unwrap_or_default();
    let root = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let opts = ParseOptions {
        prefix,
        root,
        globals: crate::repository::pathspec_globals(ctx),
        ..Default::default()
    };
    Pathspec::parse(args, &opts).map_err(|e| Die::Fatal(e.to_string()))
}

impl PathFilter {
    pub fn new(pathspec: Pathspec, decorations: Option<Decorations>) -> PathFilter {
        PathFilter {
            pathspec,
            decorations,
            trees: HashMap::new(),
        }
    }

    fn tree(&mut self, repo: &Repo<'_>, commit: &ObjectId) -> Option<ObjectId> {
        if let Some(t) = self.trees.get(commit) {
            return *t;
        }
        let t = repo.tree_of(commit);
        self.trees.insert(*commit, t);
        t
    }

    /// `id` shows no change against `parent` (the empty tree for `None`)
    /// in the paths of interest.
    pub fn treesame(
        &mut self,
        repo: &Repo<'_>,
        id: &ObjectId,
        parent: Option<&ObjectId>,
    ) -> Result<bool, ObjectId> {
        // A root commit is compared with the empty tree only.
        if parent.is_some() && self.decorations.as_ref().is_some_and(|d| d.contains(id)) {
            return Ok(false);
        }
        self.paths_same(repo, id, parent)
    }

    /// The paths of interest are the same in `id` and `parent`.
    fn paths_same(
        &mut self,
        repo: &Repo<'_>,
        id: &ObjectId,
        parent: Option<&ObjectId>,
    ) -> Result<bool, ObjectId> {
        if self.decorations.is_some() && self.pathspec.is_empty() {
            // Without paths only a root commit (against the empty tree)
            // changes something.
            return match parent {
                Some(_) => Ok(true),
                None => self.is_empty(repo, id),
            };
        }
        let a = self.tree(repo, id).ok_or(*id)?;
        let b = match parent {
            Some(p) => Some(self.tree(repo, p).ok_or(*p)?),
            None => None,
        };
        if Some(a) == b {
            return Ok(true);
        }
        let mut base = Vec::new();
        Ok(!self.differ(repo, Some(&a), b.as_ref(), &mut base)?)
    }

    /// `--remove-empty`: `parent` has no path of interest and `id` adds
    /// some.
    pub fn only_added(
        &mut self,
        repo: &Repo<'_>,
        id: &ObjectId,
        parent: &ObjectId,
    ) -> Result<bool, ObjectId> {
        Ok(!self.paths_same(repo, id, Some(parent))? && self.is_empty(repo, parent)?)
    }

    /// Commit `id` has no path of interest.
    fn is_empty(&mut self, repo: &Repo<'_>, id: &ObjectId) -> Result<bool, ObjectId> {
        let tree = self.tree(repo, id).ok_or(*id)?;
        let mut base = Vec::new();
        Ok(!self.has_match(repo, &tree, &mut base)?)
    }

    fn items(repo: &Repo<'_>, tree: Option<&ObjectId>) -> Result<Vec<Item>, ObjectId> {
        let Some(tree) = tree else {
            return Ok(Vec::new());
        };
        let (kind, data) = repo.objects.read(tree).ok().flatten().ok_or(*tree)?;
        if kind != ObjectKind::Tree {
            return Err(*tree);
        }
        let mut out = Vec::new();
        for entry in TreeIter::new(repo.objects.hash_kind(), &data) {
            let entry = entry.map_err(|_| *tree)?;
            out.push(Item {
                name: entry.name.to_vec(),
                mode: entry.mode_bits,
                id: entry.oid,
            });
        }
        Ok(out)
    }

    /// Some path of interest differs between the trees `a` and `b` (below
    /// the directory `base`, which ends in `/` unless empty).
    fn differ(
        &self,
        repo: &Repo<'_>,
        a: Option<&ObjectId>,
        b: Option<&ObjectId>,
        base: &mut Vec<u8>,
    ) -> Result<bool, ObjectId> {
        let left = Self::items(repo, a)?;
        let right = Self::items(repo, b)?;
        let mut by_name: HashMap<&[u8], &Item> = HashMap::new();
        for r in &right {
            by_name.insert(&r.name, r);
        }
        let len = base.len();
        let check = |x: Option<&Item>, y: Option<&Item>, base: &mut Vec<u8>| {
            let name = &x.or(y).expect("one side").name;
            if let (Some(x), Some(y)) = (x, y)
                && x.mode == y.mode
                && x.id == y.id
            {
                return Ok(false);
            }
            base.truncate(len);
            base.extend_from_slice(name);
            let x_tree = x.filter(|i| i.is_tree());
            let y_tree = y.filter(|i| i.is_tree());
            if x_tree.is_some() || y_tree.is_some() {
                if self.pathspec.dir_interest(base) == DirInterest::None {
                    // Only files of the same name can matter.
                    let file = x.filter(|i| !i.is_tree()).or(y.filter(|i| !i.is_tree()));
                    return Ok(file.is_some() && self.pathspec.matches(base));
                }
            }
            if let (Some(xt), Some(yt)) = (x_tree, y_tree) {
                base.push(b'/');
                return self.differ(repo, Some(&xt.id), Some(&yt.id), base);
            }
            for side in [x, y].into_iter().flatten() {
                if side.is_tree() {
                    base.truncate(len + name.len());
                    base.push(b'/');
                    if self.has_match(repo, &side.id, base)? {
                        return Ok(true);
                    }
                } else {
                    base.truncate(len + name.len());
                    if self.pathspec.matches(base) {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        };
        let mut seen: HashMap<&[u8], ()> = HashMap::new();
        for l in &left {
            seen.insert(&l.name, ());
            if check(Some(l), by_name.get(l.name.as_slice()).copied(), base)? {
                base.truncate(len);
                return Ok(true);
            }
        }
        for r in &right {
            if !seen.contains_key(r.name.as_slice()) && check(None, Some(r), base)? {
                base.truncate(len);
                return Ok(true);
            }
        }
        base.truncate(len);
        Ok(false)
    }

    /// The tree `tree` (at `base`, ending in `/` unless empty) holds a path
    /// of interest.
    fn has_match(
        &self,
        repo: &Repo<'_>,
        tree: &ObjectId,
        base: &mut Vec<u8>,
    ) -> Result<bool, ObjectId> {
        let len = base.len();
        for item in Self::items(repo, Some(tree))? {
            base.truncate(len);
            base.extend_from_slice(&item.name);
            let hit = if item.is_tree() {
                match self.pathspec.dir_interest(base) {
                    DirInterest::None => false,
                    DirInterest::All => true,
                    DirInterest::Maybe => {
                        base.push(b'/');
                        self.has_match(repo, &item.id, base)?
                    }
                }
            } else {
                self.pathspec.matches(base)
            };
            if hit {
                base.truncate(len);
                return Ok(true);
            }
        }
        base.truncate(len);
        Ok(false)
    }
}
