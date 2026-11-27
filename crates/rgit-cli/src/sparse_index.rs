// SPDX-License-Identifier: MIT
//! The sparse index (Documentation/technical/sparse-index.adoc,
//! gitformat-index(5) "Sparse Directory Entries", config/index.adoc
//! `index.sparse`, git-sparse-checkout(1) `--sparse-index`).
//!
//! On disk, a directory outside a cone-mode sparse checkout is stored as
//! one sparse directory entry. [`Repo::read_index`] expands sparse
//! directory entries from their trees (rgit-index, with the tree reader
//! registered by [`install`]); writing the repository's index collapses
//! directories again (the write transform registered by [`install`]).
//!
//! The commands git keeps sparse in memory on their hot paths (`status`,
//! `diff`, `add`, `commit`, `reset`, `checkout`/`switch` through
//! rgit-unpack) read with [`Repo::read_index_sparse`] instead: the sparse
//! directory entries stay in the in-memory index (mode `040000`, path
//! with trailing `/`, the tree, skip-worktree), so their work is
//! O(populated). Their comparisons skip a tree's directory where the index
//! has a sparse directory entry holding the same tree ([`expand_unless_same`]
//! checks that first); everything else falls back to [`expand`], which
//! expands the index in memory exactly as `read_index` would have, without
//! changing git's notion below. Writing a physically sparse index needs no
//! expansion: the cache tree is computed with the sparse directory
//! entries as leaves.
//!
//! git distinguishes a "sparse" from an "expanded" in-memory index, and
//! that difference is visible: which directories a written index keeps
//! file by file, `status` saying "You are in a sparse checkout." without a
//! percentage, `ls-files --sparse`, and the trace2 regions
//! `index/ensure_full_index` and `index/convert_to_sparse`. This module
//! keeps that state for the repository's index (independent of the
//! in-memory form above: the regions say where git expands or converts):
//!
//! - An index read from a sparse file stays logically sparse; directories
//!   the file listed file by file stay so when it is written again, new
//!   directories outside the cone are collapsed.
//! - A full index read while the sparse index is on is converted (region
//!   `convert_to_sparse`) and then treated as above.
//! - [`ensure_full`] marks the index expanded (region `ensure_full_index`),
//!   as git does for the operations that need every file entry (callers:
//!   paths inside sparse directories, see [`expand_for_path`] and
//!   [`expand_for_pathspec`]); an expanded index is converted completely
//!   when written (region `convert_to_sparse`).
//! - A sparse file read with the sparse index off is expanded and written
//!   in full.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rgit_hash::ObjectId;
use rgit_index::{CacheTree, CollapseRules, Index, SparseOrigin, TreeNode};
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};
use rgit_odb::ObjectDatabase;
use rgit_worktree::sparse::{DirMatch, Sparse};

use crate::Die;
use crate::repository::Repo;

/// The sparse-index settings of the repository.
struct Settings {
    /// The absolute path of the repository's index file.
    index: PathBuf,
    /// The work tree, for directories present despite the patterns.
    work_tree: Option<PathBuf>,
    /// `index.sparse` (or `GIT_TEST_SPARSE_INDEX`) with a cone-mode sparse
    /// checkout and no split index: the patterns.
    cone: Option<Sparse>,
    /// `sparse.expectFilesOutsideOfPatterns`.
    expect_files_outside: bool,
    /// The advice settings (`advice.sparseIndexExpanded`).
    advisor: Option<rgit_messages::Advisor>,
    /// The object database (hash algorithm of the trees computed when
    /// collapsing).
    odb: Rc<ObjectDatabase>,
}

#[derive(Default)]
struct State {
    settings: Option<Settings>,
    /// The in-memory index is sparse in git's sense.
    sparse: bool,
    /// The layout of the sparse index (as read, or as converted on read).
    origin: Option<SparseOrigin>,
    /// [`note_lookup`] reported the expansion.
    noted: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// `GIT_TEST_SPARSE_INDEX`, the test suite's switch for the sparse index.
pub fn test_env() -> Option<bool> {
    let v = std::env::var("GIT_TEST_SPARSE_INDEX").ok()?;
    if v.is_empty() {
        return None;
    }
    rgit_config::value::parse_bool(Some(v.as_bytes()))
}

/// `index.sparse` (overridden by `GIT_TEST_SPARSE_INDEX`).
pub fn configured(config: &rgit_config::ConfigSet) -> bool {
    test_env().unwrap_or_else(|| config.bool("index.sparse").ok().flatten().unwrap_or(false))
}

/// The cone patterns when the repository's index is to be sparse.
fn cone_patterns(repo: &Repo) -> Option<Sparse> {
    if !configured(&repo.config)
        || crate::index_config::split_setting(&repo.config) == Some(true)
        || !crate::sparse::cone_config(&repo.config).unwrap_or(false)
    {
        return None;
    }
    crate::sparse::load(repo).filter(Sparse::is_cone)
}

/// Registers the tree reader expanding sparse indexes when they are read,
/// for the first repository the process sets up.
pub fn install_reader(setup: &rgit_repo::Setup) {
    if rgit_index::has_tree_reader() {
        return;
    }
    let objects = rgit_odb::objects_dir(&setup.cwd.join(&setup.common_dir));
    let hash = setup.format.object_format;
    let odb: std::cell::OnceCell<ObjectDatabase> = std::cell::OnceCell::new();
    // Indexes of other repositories (submodules) next to their objects.
    let others: RefCell<Vec<(PathBuf, ObjectDatabase)>> = RefCell::new(Vec::new());
    let read = |db: &ObjectDatabase, id: &ObjectId| match db.read(id) {
        Ok(Some(raw)) if raw.kind == ObjectKind::Tree => Some(raw.data.to_vec()),
        _ => None,
    };
    rgit_index::set_tree_reader(Some(Box::new(move |index: &Path, id: &ObjectId| {
        let main = odb.get_or_init(|| {
            ObjectDatabase::open_with_packs(&objects, hash, rgit_odb::LooseOptions::default())
        });
        if let Some(data) = read(main, id) {
            return Some(data);
        }
        let dir = index.parent()?.join("objects");
        if !dir.is_dir() || dir == objects {
            return None;
        }
        let mut others = others.borrow_mut();
        if !others.iter().any(|(d, _)| *d == dir) {
            let db = ObjectDatabase::open_with_packs(&dir, hash, rgit_odb::LooseOptions::default());
            others.push((dir.clone(), db));
        }
        let (_, db) = others.iter().find(|(d, _)| *d == dir)?;
        read(db, id)
    })));
}

/// Registers the write transform (collapsing the repository's index) for
/// `repo` and sets up the sparse-index state.
///
/// Opening the same repository again keeps the state of the first opening
/// (a command may open it several times).
pub fn install(repo: &Repo) {
    let index = absolute(&repo.setup.cwd.join(repo.index_path()));
    let same = STATE.with(|s| {
        s.borrow()
            .settings
            .as_ref()
            .is_some_and(|st| st.index == index)
    });
    if same {
        return;
    }
    let hash = repo.hash;
    let objects = rgit_odb::objects_dir(&repo.setup.cwd.join(&repo.setup.common_dir));
    let odb = Rc::new(ObjectDatabase::open_with_packs(
        objects,
        hash,
        rgit_odb::LooseOptions::default(),
    ));
    let advisor =
        rgit_messages::Advisor::new(&repo.config, &rgit_config::SystemEnv::default(), false).ok();
    let settings = Settings {
        index,
        work_tree: repo
            .setup
            .work_tree
            .as_ref()
            .map(|w| repo.setup.cwd.join(w)),
        cone: cone_patterns(repo),
        expect_files_outside: repo
            .config
            .bool("sparse.expectfilesoutsideofpatterns")
            .ok()
            .flatten()
            .unwrap_or(false),
        advisor,
        odb,
    };
    STATE.with(|s| {
        *s.borrow_mut() = State {
            settings: Some(settings),
            ..State::default()
        };
    });
    rgit_index::set_write_transform(Some(Box::new(transform)));
}

/// Sets the patterns the index is written for after `git sparse-checkout`
/// changed them or `index.sparse` (`None`: write a full index); the next
/// write converts the whole index.
pub fn set_patterns(repo: &Repo, cone: Option<Sparse>) {
    let cone = cone.filter(|_| crate::index_config::split_setting(&repo.config) != Some(true));
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(settings) = &mut s.settings {
            settings.cone = cone;
        }
        s.sparse = false;
        s.origin = None;
    });
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn region(label: &str) {
    rgit_trace::trace2::region_enter("index", label, None, true);
    rgit_trace::trace2::region_leave();
}

/// Whether the repository's in-memory index is sparse (git's notion; see
/// the module documentation).
pub fn is_sparse() -> bool {
    STATE.with(|s| s.borrow().sparse)
}

/// The directories the logically sparse index holds as sparse directory
/// entries (with trailing slash); empty when it is not sparse.
pub fn sparse_dirs() -> Vec<Vec<u8>> {
    STATE.with(|s| {
        let s = s.borrow();
        match (&s.origin, s.sparse) {
            (Some(o), true) => o.dirs.clone(),
            _ => Vec::new(),
        }
    })
}

/// Called by [`Repo::read_index`] with the freshly read (and expanded)
/// repository index. Returns whether the index changed.
pub fn after_read(index: &mut Index) -> bool {
    let origin = index.sparse_origin().cloned();
    index.set_sparse_origin(None);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let state = &mut *s;
        let Some(settings) = &state.settings else {
            return false;
        };
        let Some(cone) = &settings.cone else {
            if origin.is_some() {
                region("ensure_full_index");
            }
            state.sparse = false;
            state.origin = None;
            return false;
        };
        let origin = match origin {
            Some(o) => o,
            None => {
                // A full index converted to a sparse one on reading.
                region("convert_to_sparse");
                match collapse(settings, cone, index, None) {
                    Some(c) => SparseOrigin::of(&c),
                    None => SparseOrigin::of(index),
                }
            }
        };
        // Sparse directories present in the work tree: git expands the
        // index to look at their files.
        let present = !settings.expect_files_outside
            && settings.work_tree.as_ref().is_some_and(|wt| {
                origin.dirs.iter().any(|d| {
                    let d = d.strip_suffix(b"/").unwrap_or(d);
                    std::fs::symlink_metadata(os_path(wt, d)).is_ok_and(|m| m.is_dir())
                })
            });
        if present {
            if let Some(advisor) = &settings.advisor {
                let _ = advisor.advise(
                    rgit_messages::Advice::SparseIndexExpanded,
                    crate::messages::sparse_checkout::SPARSE_INDEX_EXPANDED_HINT,
                );
            }
            region("ensure_full_index");
            state.sparse = false;
            state.origin = None;
        } else {
            state.sparse = true;
            state.origin = Some(origin);
        }
        false
    })
}

/// Called by [`Repo::read_index_sparse`] with the index as the file holds
/// it (sparse directory entries kept). Sets up the state like
/// [`after_read`]; the sparse directory entries stay in memory only while
/// the index is sparse in git's sense and none of their directories is
/// present in the work tree, otherwise the index is expanded (see
/// [`expand`]).
pub fn after_sparse_read(repo: &Repo, index: &mut Index) -> Result<(), Die> {
    if !index.is_sparse_index() {
        return Ok(());
    }
    let present = || {
        STATE.with(|s| {
            let s = s.borrow();
            let Some(wt) = s.settings.as_ref().and_then(|st| st.work_tree.as_ref()) else {
                return false;
            };
            index.entries().iter().filter(|e| e.mode.is_dir()).any(|e| {
                let d = e.path().strip_suffix(b"/").unwrap_or(e.path());
                std::fs::symlink_metadata(os_path(wt, d)).is_ok()
            })
        })
    };
    if !index.is_sparse() || !is_sparse() || present() {
        expand(repo, index)?;
        // What `read_index` does with the expanded files.
        if let Some(wt) = &repo.setup.work_tree {
            let root = repo.setup.cwd.join(wt);
            crate::sparse::clear_present(&repo.config, &root, index);
        }
    }
    Ok(())
}

/// Called by [`Repo::read_index_sparse`] right after reading: records the
/// layout of a sparse index file as [`after_read`] expects it.
pub fn before_sparse_read(index: &mut Index) {
    if index.is_sparse_index() {
        index.set_sparse_origin(Some(SparseOrigin::of(index)));
    }
}

/// Replaces the sparse directory entries of `index` (read with
/// [`Repo::read_index_sparse`]) by the files of their trees, as
/// [`Repo::read_index`] would have read them. Unlike [`ensure_full`]
/// this is invisible: whether the index is sparse in git's sense does not
/// change (rgit's own fallback for work not done on sparse directory
/// entries).
pub fn expand(repo: &Repo, index: &mut Index) -> Result<(), Die> {
    if !index.is_sparse_index() {
        return Ok(());
    }
    let mut read = |id: &ObjectId| repo.read_tree_data(id).map(|d| d.to_vec());
    // A missing tree fails as the expanding read does.
    index
        .expand_sparse(&mut read)
        .map_err(|e| repo.index_error(&e))?;
    index.set_sparse_origin(None);
    Ok(())
}

/// [`expand`] unless every sparse directory entry of `index` names the
/// tree `tree` (`None`: the empty tree) has at its path: then comparing
/// `index` with `tree` can skip those directories.
pub fn expand_unless_same(
    repo: &Repo,
    index: &mut Index,
    tree: Option<ObjectId>,
) -> Result<(), Die> {
    if !index.is_sparse() {
        return Ok(());
    }
    let differs = index.entries().iter().filter(|e| e.mode.is_dir()).any(|e| {
        let dir = e.path().strip_suffix(b"/").unwrap_or(e.path());
        tree.and_then(|t| tree_entry(repo, t, dir)) != Some(e.id)
    });
    if differs {
        expand(repo, index)?;
    }
    Ok(())
}

/// git's `ensure_full_index()`: the in-memory index stops being sparse;
/// it is converted completely when written.
pub fn ensure_full() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.sparse {
            region("ensure_full_index");
            s.sparse = false;
            s.origin = None;
        }
    });
}

/// Expands (see [`ensure_full`]) when `path` lies inside a sparse
/// directory entry of the logically sparse index.
pub fn expand_for_path(path: &[u8]) {
    let inside = STATE.with(|s| {
        let s = s.borrow();
        s.sparse
            && s.origin
                .as_ref()
                .is_some_and(|o| o.dir_containing(path).is_some_and(|d| d.len() < path.len()))
    });
    if inside {
        ensure_full();
    }
}

/// Expands when a pathspec item may match paths inside a sparse
/// directory without matching the whole directory: a literal path below
/// one, or a wildcard pattern whose fixed leading part reaches into one or
/// that could match below one. An empty pathspec matches everything and
/// expands.
pub fn expand_for_pathspec(pathspec: &rgit_pathspec::Pathspec) {
    let dirs = sparse_dirs();
    if dirs.is_empty() {
        return;
    }
    let items: Vec<_> = pathspec
        .items()
        .iter()
        .filter(|i| !i.is_exclude())
        .collect();
    if items.is_empty() {
        ensure_full();
        return;
    }
    if reaches_inside(&dirs, pathspec) {
        ensure_full();
    }
}

/// Whether `pathspec` may match paths inside a sparse directory entry of
/// `index` without matching the whole directory (see
/// [`expand_for_pathspec`]; an empty pathspec matches whole directories).
pub fn pathspec_reaches_inside(index: &Index, pathspec: &rgit_pathspec::Pathspec) -> bool {
    if !index.is_sparse() {
        return false;
    }
    // Excluded paths and case-folded literals: decided file by file.
    if pathspec
        .items()
        .iter()
        .any(|i| i.is_exclude() || i.magic().contains(rgit_pathspec::Magic::ICASE))
    {
        return true;
    }
    let dirs: Vec<Vec<u8>> = index
        .entries()
        .iter()
        .filter(|e| e.mode.is_dir())
        .map(|e| e.path().to_vec())
        .collect();
    reaches_inside(&dirs, pathspec)
}

fn reaches_inside(dirs: &[Vec<u8>], pathspec: &rgit_pathspec::Pathspec) -> bool {
    let items: Vec<_> = pathspec
        .items()
        .iter()
        .filter(|i| !i.is_exclude())
        .collect();
    items.iter().any(|item| {
        let pat = item.pattern();
        let fixed = &pat[..item.nowildcard_len().min(pat.len())];
        let wild = fixed.len() < pat.len();
        let flags = if item.magic().contains(rgit_pathspec::Magic::ICASE) {
            rgit_pathspec::wildmatch::WildmatchFlags::CASEFOLD
        } else {
            rgit_pathspec::wildmatch::WildmatchFlags::NONE
        };
        dirs.iter().any(|d| {
            if !wild {
                // A literal path below the directory.
                return fixed.starts_with(d) && fixed.len() > d.len();
            }
            if !fixed.starts_with(d) && !d.starts_with(fixed) {
                return false;
            }
            // The pattern may reach into the directory: fine when it
            // matches the directory itself as a whole.
            let dir = d.strip_suffix(b"/").unwrap_or(d);
            !(rgit_pathspec::wildmatch::wildmatch(pat, d, flags)
                || rgit_pathspec::wildmatch::wildmatch(pat, dir, flags))
        })
    })
}

/// For a lookup of `path` in an index read with [`Index::read_file`]
/// outside [`Repo::read_index`] (`:<path>` revisions): expands when the
/// index file stored `path` inside a sparse directory entry.
pub fn note_lookup(index: &Index, path: &[u8]) {
    let inside = index
        .sparse_origin()
        .is_some_and(|o| o.dir_containing(path).is_some_and(|d| d.len() < path.len()));
    if !inside {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.noted {
            s.noted = true;
            drop(s);
            region("ensure_full_index");
        }
    });
}

/// Expands when a sparse directory of the logically sparse index differs
/// from the same directory in `tree` (`None`: the empty tree), as the
/// patch modes comparing the index with a tree need its files.
pub fn expand_if_differs(repo: &Repo, index: &Index, tree: Option<ObjectId>) {
    let Some(view) = sparse_view(index) else {
        return;
    };
    let differs = view.entries().iter().filter(|e| e.mode.is_dir()).any(|e| {
        let dir = e.path().strip_suffix(b"/").unwrap_or(e.path());
        tree.and_then(|t| tree_entry(repo, t, dir)) != Some(e.id)
    });
    if differs {
        ensure_full();
    }
}

/// The object at `path` below `tree`.
fn tree_entry(repo: &Repo, tree: ObjectId, path: &[u8]) -> Option<ObjectId> {
    let mut cur = tree;
    for comp in path.split(|&b| b == b'/') {
        let data = repo.read_tree_data(&cur)?;
        let entry = rgit_object::TreeIter::new(repo.hash, &data)
            .filter_map(Result::ok)
            .find(|e| e.name == comp)?;
        cur = entry.oid;
    }
    Some(cur)
}

/// The index as the logically sparse index has it (sparse directory
/// entries where git would have them), for `ls-files --sparse`.
pub fn sparse_view(index: &Index) -> Option<Index> {
    STATE.with(|s| {
        let s = s.borrow();
        if !s.sparse {
            return None;
        }
        let settings = s.settings.as_ref()?;
        let cone = settings.cone.as_ref()?;
        collapse(settings, cone, index, s.origin.as_ref())
    })
}

/// The write transform: the repository's index collapsed as described in
/// the module documentation.
fn transform(path: &Path, index: &Index) -> Option<Index> {
    STATE.with(|s| {
        let s = s.borrow();
        let settings = s.settings.as_ref()?;
        // Sparse directory entries kept in memory (`read_index_sparse`)
        // are expanded where an expanded index would be written as it is.
        let expanded = || index.is_sparse().then(|| expanded_copy(settings, index))?;
        if absolute(path) != settings.index {
            return expanded();
        }
        let Some(cone) = settings.cone.as_ref() else {
            return expanded();
        };
        if !s.sparse {
            region("convert_to_sparse");
        }
        let origin = if s.sparse { s.origin.as_ref() } else { None };
        collapse(settings, cone, index, origin).or_else(expanded)
    })
}

/// `index` with its sparse directory entries expanded (`None` when a tree
/// is missing).
fn expanded_copy(settings: &Settings, index: &Index) -> Option<Index> {
    let mut copy = index.clone();
    let mut read = |id: &ObjectId| match settings.odb.read(id) {
        Ok(Some(raw)) if raw.kind == ObjectKind::Tree => Some(raw.data.to_vec()),
        _ => None,
    };
    copy.expand_sparse(&mut read).ok()?;
    copy.set_sparse_origin(None);
    Some(copy)
}

/// `index` with the cache tree brought up to date and directories outside
/// the cone collapsed, keeping those `origin` lists file by file. `None`
/// when no cache tree can be computed (conflicts).
fn collapse(
    settings: &Settings,
    cone: &Sparse,
    index: &Index,
    origin: Option<&SparseOrigin>,
) -> Option<Index> {
    let tree = build_cache_tree(&settings.odb, index)?;
    let mut full = index.clone();
    full.set_cache_tree(Some(tree));
    let outside = |d: &[u8]| cone.dir_match(d) == Some(DirMatch::Excluded);
    let keep = |d: &[u8]| origin.is_some_and(|o| o.has_file_below(d));
    let collapsed = full.collapse_sparse(&CollapseRules {
        outside: &outside,
        keep: &keep,
    });
    // The trees of sparse directory entries are written (git leaves the
    // other trees of the cache tree unwritten).
    for e in collapsed.entries().iter().filter(|e| e.mode.is_dir()) {
        if matches!(settings.odb.header(&e.id), Ok(Some(_))) {
            continue;
        }
        let dir = e.path().strip_suffix(b"/").unwrap_or(e.path());
        let below: Vec<&rgit_index::Entry> = index.entries()[index.dir_range(dir)].iter().collect();
        if matches!(below.as_slice(), [d] if d.mode.is_dir()) {
            // Kept from the index as read: its tree is not ours to write.
            continue;
        }
        let name = dir.rsplit(|&b| b == b'/').next().unwrap_or(dir);
        build(&settings.odb, &below, e.path().len(), name, true)?;
    }
    Some(collapsed)
}

/// The cache tree for `index`, with the ids of the trees it would write
/// (none are written; objects the index names need not exist). A directory holding intent-to-add
/// entries gets an invalid node. `None` with unmerged entries.
fn build_cache_tree(odb: &ObjectDatabase, index: &Index) -> Option<CacheTree> {
    if index.has_conflicts() {
        return None;
    }
    let entries: Vec<&rgit_index::Entry> = index.entries().iter().collect();
    let (_, root) = build(odb, &entries, 0, b"", false)?;
    Some(CacheTree { root })
}

/// The tree (and cache tree node) of `entries`, whose paths share the
/// first `strip` bytes; with `write` the trees are written to `odb`.
fn build(
    odb: &ObjectDatabase,
    entries: &[&rgit_index::Entry],
    strip: usize,
    name: &[u8],
    write: bool,
) -> Option<(ObjectId, TreeNode)> {
    let mut tree = Tree::new();
    let mut children = Vec::new();
    let mut ita = false;
    let mut i = 0;
    while i < entries.len() {
        let rest = &entries[i].path()[strip..];
        match rest.iter().position(|&c| c == b'/') {
            Some(slash) if slash + 1 < rest.len() => {
                let dir = &rest[..=slash];
                let mut j = i + 1;
                while j < entries.len() && entries[j].path()[strip..].starts_with(dir) {
                    j += 1;
                }
                let (id, node) = build(
                    odb,
                    &entries[i..j],
                    strip + slash + 1,
                    &rest[..slash],
                    write,
                )?;
                children.push(node);
                tree.entries.push(TreeEntry::new(
                    EntryMode::from_octal(b"40000")?,
                    &rest[..slash],
                    id,
                ));
                i = j;
            }
            _ => {
                let e = entries[i];
                if e.flags.intent_to_add {
                    ita = true;
                } else {
                    let (name, mode) = if e.mode.is_dir() {
                        // A sparse directory entry: a cache tree leaf
                        // covering this one entry.
                        let dir = rest.strip_suffix(b"/").unwrap_or(rest);
                        children.push(TreeNode::new_valid(dir.to_vec(), 1, e.id));
                        (dir, b"40000".to_vec())
                    } else {
                        (rest, format!("{:o}", e.mode.0).into_bytes())
                    };
                    tree.entries
                        .push(TreeEntry::new(EntryMode::from_octal(&mode)?, name, e.id));
                }
                i += 1;
            }
        }
    }
    tree.entries.sort_by(TreeEntry::cmp_tree_order);
    let data = tree.serialize();
    let id = if write {
        odb.write(ObjectKind::Tree, &data).ok()?
    } else {
        rgit_hash::hash_object(odb.hash_kind(), "tree", &data).ok()?
    };
    children.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
    let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let mut node = if ita {
        TreeNode::new_invalid(name.to_vec())
    } else {
        TreeNode::new_valid(name.to_vec(), count, id)
    };
    node.children = children;
    Some((id, node))
}

fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
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
