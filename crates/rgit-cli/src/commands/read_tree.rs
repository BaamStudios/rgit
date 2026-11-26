// SPDX-License-Identifier: MIT
//! `git read-tree` (Documentation/git-read-tree.adoc): `read-tree <tree-ish>`
//! (replaces the index and primes the cache tree), `--empty`, and the one-
//! and two-way merges `-m`/`--reset` with `-u`, `-i`, `-n` and
//! `--index-output` through the unpack engine (`rgit-unpack`, #107),
//! `--prefix=<dir>/` and several trees without `-m` (an overlay, later
//! trees win), and three-way merges (`-m <base>... <head> <remote>`,
//! `--aggressive`).

use std::path::{Path, PathBuf};

use rgit_fs::{LockError, LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_index::{AddOptions, CacheTree, Entry, Index, TreeNode};
use rgit_object::ObjectKind;
use rgit_unpack::{Merge, Options, PathOptions, Worktree};
use rgit_worktree::stat::smudge_racy_entries;

use crate::messages::read_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

const MODE_NONE: u8 = 0;
const MODE_MERGE: u8 = 1;
const MODE_RESET: u8 = 2;
const MODE_PREFIX: u8 = 3;

/// `core.protectNTFS` and `core.protectHFS`.
pub fn path_options(repo: &Repo) -> PathOptions {
    let d = PathOptions::default();
    PathOptions {
        protect_ntfs: repo
            .config
            .bool("core.protectntfs")
            .ok()
            .flatten()
            .unwrap_or(d.protect_ntfs),
        protect_hfs: repo
            .config
            .bool("core.protecthfs")
            .ok()
            .flatten()
            .unwrap_or(d.protect_hfs),
    }
}

/// Maps a failure to take `index.lock` to git's messages.
pub fn lock_failed(e: LockError) -> Die {
    if let Some(w) = e.warning() {
        eprintln!("warning: {w}");
    }
    Die::Fatal(e.to_string())
}

/// Maps an engine error to git's output: `error:` lines exit with 128.
pub fn unpack_failed(e: rgit_unpack::Error) -> Die {
    match e {
        rgit_unpack::Error::Rejected(r) => {
            for m in r.plumbing_messages() {
                eprintln!("error: {m}");
            }
            Die::Exit(128)
        }
        e @ (rgit_unpack::Error::InvalidPath(_) | rgit_unpack::Error::MissingBlob(_)) => {
            Die::Error(e.to_string(), 128)
        }
        e => Die::Fatal(e.to_string()),
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let empty = Cell::new(false);
    let verbose = Cell::new(0);
    let trivial = Cell::new(false);
    let aggressive = Cell::new(false);
    let update = Cell::new(false);
    let index_only = Cell::new(false);
    let dry_run = Cell::new(0);
    let sparse = Cell::new(false);
    let no_sparse = Cell::new(false);
    let debug_unpack = Cell::new(false);
    let recurse = Cell::new(None::<bool>);
    let super_prefix = RefCell::new(None::<String>);
    let quiet = Cell::new(0);
    let index_output = RefCell::new(None);
    let mode = Cell::new(MODE_NONE);
    let prefix = RefCell::new(None::<String>);
    let exclude = RefCell::new(None::<String>);
    let set_mode = |m: u8| {
        if mode.get() != MODE_NONE && mode.get() != m {
            return Err(CallbackError::Fatal(msg::WHICH_ONE.into()));
        }
        mode.set(m);
        Ok(())
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string(None, "index-output", &index_output, "file", msg::HELP_INDEX_OUTPUT)
                .noneg(),
            Opt::flag(None, "empty", &empty, msg::HELP_EMPTY),
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::group(msg::GROUP_MERGING),
            Opt::callback('m', None, None, msg::HELP_MERGE, |_, _| set_mode(MODE_MERGE)).noarg(),
            Opt::flag(None, "trivial", &trivial, msg::HELP_TRIVIAL),
            Opt::flag(None, "aggressive", &aggressive, msg::HELP_AGGRESSIVE),
            Opt::callback(None, "reset", None, msg::HELP_RESET, |_, _| set_mode(MODE_RESET))
                .noarg(),
            Opt::callback(None, "prefix", Some("<subdirectory>/"), msg::HELP_PREFIX, |v, _| {
                set_mode(MODE_PREFIX)?;
                *prefix.borrow_mut() = v.map(str::to_string);
                Ok(())
            })
            .literal_argh()
            .noneg(),
            Opt::flag('u', None, &update, msg::HELP_UPDATE),
            Opt::callback(
                None,
                "exclude-per-directory",
                Some("gitignore"),
                msg::HELP_EXCLUDE,
                |v, _| {
                    if !update.get() {
                        return Err(CallbackError::Fatal(msg::EXCLUDE_NEEDS_U.into()));
                    }
                    if v != Some(".gitignore") {
                        return Err(CallbackError::Fatal(msg::EXCLUDE_GITIGNORE.into()));
                    }
                    *exclude.borrow_mut() = v.map(str::to_string);
                    Ok(())
                },
            )
            .noneg(),
            Opt::flag('i', None, &index_only, msg::HELP_INDEX_ONLY),
            Opt::dry_run(&dry_run, msg::HELP_DRY_RUN),
            Opt::flag(None, "no-sparse-checkout", &no_sparse, msg::HELP_NO_SPARSE).noneg(),
            Opt::flag(None, "sparse-checkout", &sparse, msg::HELP_SPARSE).noneg(),
            Opt::flag(None, "debug-unpack", &debug_unpack, msg::HELP_DEBUG_UNPACK),
            crate::recurse_submodules::option(&recurse, msg::HELP_RECURSE),
            Opt::string(None, "super-prefix", &super_prefix, "path", "").hidden(),
            Opt::quiet(&quiet, msg::HELP_QUIET),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let _ = (trivial.get(), verbose.get());

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let recurse = crate::recurse_submodules::set(&repo, recurse.get());
    let super_prefix = super_prefix.borrow().clone();
    let mut trees = Vec::new();
    for spec in &args {
        if trees.len() >= 8 {
            return Err(Die::Fatal(msg::TOO_MANY_TREES.into()));
        }
        let id = repo
            .resolve(spec)
            .and_then(|id| repo.peel_to(id, ObjectKind::Tree))
            .ok_or_else(|| Die::Fatal(msg::not_valid_object_name(spec)))?;
        trees.push(id);
    }
    let repo = repo;
    let mode = mode.get();
    let (update, index_only) = (update.get(), index_only.get());
    if mode == MODE_NONE && trees.is_empty() && !empty.get() {
        eprintln!("warning: {}", msg::EMPTY_DEPRECATED);
    }
    if update && index_only {
        return Err(Die::Fatal(msg::U_AND_I.into()));
    }
    if mode == MODE_NONE && update {
        return Err(Die::Fatal(msg::U_MEANINGLESS.into()));
    }
    if mode == MODE_NONE && index_only {
        return Err(Die::Fatal(msg::I_MEANINGLESS.into()));
    }
    if (mode == MODE_MERGE || mode == MODE_RESET) && trees.is_empty() {
        return Err(Die::Fatal(msg::NEED_TREE.into()));
    }
    if empty.get() && !trees.is_empty() {
        return Err(Die::Fatal(msg::EMPTY_CONTRADICTS.into()));
    }
    if mode == MODE_PREFIX && prefix.borrow().as_deref().is_some_and(|p| p.starts_with('/')) {
        return Err(Die::Fatal(msg::PREFIX_SLASH.into()));
    }
    if mode == MODE_RESET && trees.len() > 2 {
        return Err(Die::Fatal(msg::THREE_WAY_UNSUPPORTED.into()));
    }

    let index_path = repo.setup.cwd.join(repo.index_path());
    let mut lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    // Without -m the old index does not matter (an empty file is fine).
    let (old, old_mtime) = if mode == MODE_NONE {
        let path = repo.setup.cwd.join(repo.index_path());
        match std::fs::metadata(&path) {
            Ok(m) if m.len() == 0 => (Index::new(repo.hash), None),
            _ => repo.read_index()?,
        }
    } else {
        repo.read_index()?
    };
    let paths = path_options(&repo);

    let index = if mode == MODE_NONE && trees.len() > 1 {
        read_overlay(&repo, &old, &trees, paths)?
    } else if mode == MODE_NONE {
        read_plain(&repo, &old, trees.first(), paths)?
    } else if mode == MODE_PREFIX {
        // Two trees: a two-way merge below the prefix, which adds what
        // the second tree has new or changed.
        let (base, tree) = match trees[..] {
            [t] => (None, t),
            [b, t] => (Some(b), t),
            [] => return Err(Die::Fatal(msg::NEED_TREE.into())),
            _ => return Err(Die::Fatal(msg::PREFIX_TREES_UNSUPPORTED.into())),
        };
        let dir = prefix.borrow().clone().unwrap_or_default();
        let (mut index, added) = read_prefix(&repo, &old, base.as_ref(), &tree, &dir, paths)?;
        if dry_run.get() != 0 {
            return Ok(0);
        }
        if update {
            check_out_new(&repo, &mut index, &added, old_mtime)?;
        }
        index
    } else {
        let merge = match trees[..] {
            [t] => Merge::OneWay(t),
            [old, new] => Merge::TwoWay { old, new },
            [ref bases @ .., head, remote] => Merge::ThreeWay {
                bases: bases.to_vec(),
                head,
                remote,
            },
            [] => unreachable!("checked above"),
        };
        let root = repo.setup.work_tree.as_ref().map(|w| repo.setup.cwd.join(w));
        let checker = match &root {
            Some(_) if !index_only => Some(repo.checker(old_mtime)?),
            _ => None,
        };
        let mut excludes = match (&root, update) {
            (Some(r), true) => Some(super::add::build_excludes(&repo, r)),
            _ => None,
        };
        let sparse_patterns = if no_sparse.get() && !sparse.get() {
            None
        } else {
            crate::sparse::load(&repo)
        };
        let mut opts = Options {
            sparse: sparse_patterns.as_ref(),
            reset: mode == MODE_RESET,
            discard_unmerged: false,
            update,
            index_only,
            show_all_errors: false,
            initial_checkout: old.is_empty() && old_mtime.is_none(),
            aggressive: aggressive.get(),
            paths,
            excludes: excludes.as_mut(),
            submodules: None,
            visible: &[],
            sparse_dirs: &[],
        };
        let updater = if recurse {
            crate::recurse_submodules::Updater::new(&repo, super_prefix.as_deref())
        } else {
            None
        };
        if let Some(u) = &updater {
            opts.submodules = Some(u);
        }
        let wt = match (&root, &checker) {
            (Some(root), Some(checker)) => Some(Worktree { root, checker }),
            _ => None,
        };
        let plan = rgit_unpack::plan(repo.odb(), &old, &merge, wt.as_ref(), &mut opts)
            .map_err(|e| match e {
                // --quiet: the refusal without its messages.
                rgit_unpack::Error::Rejected(_) if quiet.get() > 0 => Die::Exit(128),
                // Inside a submodule: paths as the superproject shows them.
                rgit_unpack::Error::Rejected(r) if super_prefix.is_some() => {
                    let prefix = super_prefix.as_deref().unwrap_or("").as_bytes();
                    unpack_failed(rgit_unpack::Error::Rejected(r.with_prefix(prefix)))
                }
                e => unpack_failed(e),
            })?;
        if let Merge::OneWay(t) = &merge {
            if opts.reset && check_cache_tree() {
                let listed = rgit_unpack::tree_entries(repo.odb(), t).map_err(unpack_failed)?;
                if listed.len() != plan.index().len() {
                    eprintln!("error: {}", msg::CORRUPTED_CACHE_TREE);
                    return Err(Die::Fatal(msg::UNABLE_TO_WRITE_INDEX.into()));
                }
            }
        }
        if dry_run.get() != 0 {
            return Ok(0);
        }
        match (&wt, update) {
            (Some(wt), true) => {
                let conv = crate::convert::Converter::checkout(&repo, plan.index())?;
                let applied = match &updater {
                    Some(u) => plan.apply_submodules(repo.odb(), wt, &conv, false, u, opts.reset),
                    None => plan.apply(repo.odb(), wt, &conv),
                };
                crate::sparse::print_warnings(&applied.sparse_warnings, false);
                let mut index = applied.index;
                repo.converter()?.use_index(&index);
                if let (Some(reference), Some(checker)) = (old_mtime, &checker) {
                    let _ = smudge_racy_entries(&mut index, reference, checker);
                }
                if !applied.errors.is_empty() {
                    for e in &applied.errors {
                        eprintln!("error: {e}");
                    }
                    write_index(&repo, &index, &mut lock, index_output.borrow().as_deref())?;
                    lock.commit().map_err(|_| Die::Fatal(msg::UNABLE_TO_WRITE_INDEX.into()))?;
                    return Err(Die::Exit(128));
                }
                index
            }
            _ => plan.into_index(),
        }
    };
    if dry_run.get() != 0 {
        return Ok(0);
    }
    let out = index_output.borrow().clone();
    write_index(&repo, &index, &mut lock, out.as_deref())?;
    match out {
        Some(_) => lock.rollback(),
        None => lock
            .commit()
            .map_err(|_| Die::Fatal(msg::UNABLE_TO_WRITE_INDEX.into()))?,
    }
    Ok(0)
}

/// Writes `index` into the held lock, or with `--index-output` to that
/// file (through its own lock file).
fn write_index(
    repo: &Repo,
    index: &Index,
    lock: &mut LockFile,
    output: Option<&str>,
) -> Result<(), Die> {
    let opts = crate::index_config::write_options(&repo.config, index)?;
    let fail = || Die::Fatal(msg::UNABLE_TO_WRITE_INDEX.into());
    // git refuses entries with a null object id unless GIT_ALLOW_NULL_SHA1
    // is set, which turns the error into a warning.
    let allow_null = std::env::var_os("GIT_ALLOW_NULL_SHA1").is_some();
    for e in index.entries() {
        if e.id.is_null() {
            let m = msg::null_sha1_entry(&String::from_utf8_lossy(e.path()));
            if allow_null {
                eprintln!("warning: {m}");
            } else {
                eprintln!("error: {m}");
                return Err(fail());
            }
        }
    }
    match output {
        Some(path) => {
            let path: PathBuf = repo.setup.cwd.join(Path::new(path));
            index
                .write_file(&path, &LockOptions::default(), &opts)
                .map_err(|_| fail())
        }
        None => index.write_locked(lock, &opts).map_err(|_| fail()),
    }
}

/// The entries of `tree` below `dir` (empty or ending in `/`), checked like
/// paths that are checked out.
fn entries_of(
    repo: &Repo,
    tree: &ObjectId,
    dir: &[u8],
    paths: PathOptions,
) -> Result<Vec<Entry>, Die> {
    let mut entries = rgit_unpack::tree_entries(repo.odb(), tree).map_err(unpack_failed)?;
    if !dir.is_empty() {
        for e in &mut entries {
            let mut full = dir.to_vec();
            full.extend_from_slice(e.path());
            *e = Entry::new(full, e.stage(), e.mode, e.id);
        }
    }
    for e in &entries {
        if !rgit_unpack::verify_checkout_path(e.path(), e.mode, paths) {
            return Err(unpack_failed(rgit_unpack::Error::InvalidPath(e.path().to_vec())));
        }
    }
    Ok(entries)
}

/// Adds `e` like git's read-tree does when it binds trees: an entry that
/// exists already is replaced, a file where the index has a directory is
/// left out, a directory where the index has a file replaces that file.
fn bind(index: &mut Index, e: Entry) -> Result<(), Die> {
    let mut dir = e.path().to_vec();
    dir.push(b'/');
    let pos = index.position(&dir, rgit_index::Stage::MERGED).unwrap_or_else(|p| p);
    if index
        .entries()
        .get(pos)
        .is_some_and(|x| x.path().starts_with(&dir))
    {
        return Ok(());
    }
    let opts = AddOptions {
        ok_to_replace: true,
        skip_path_check: true,
        ..AddOptions::default()
    };
    index.add(e, opts).map_err(|err| Die::Fatal(err.to_string()))
}

/// `read-tree <tree> <tree>...` without `-m`: the trees one over the other
/// (a path takes the version of the last tree that has it).
fn read_overlay(
    repo: &Repo,
    old: &Index,
    trees: &[ObjectId],
    paths: PathOptions,
) -> Result<Index, Die> {
    let mut index = old.clone();
    index.clear_entries();
    index.clear_resolve_undo();
    index.set_cache_tree(None);
    let untracked = index.take_untracked_cache();
    for tree in trees {
        for e in entries_of(repo, tree, b"", paths)? {
            bind(&mut index, e)?;
        }
    }
    index.set_untracked_cache(untracked);
    // A directory is valid where its tree exists already.
    let cache = rgit_unpack::repair_cache_tree(&index, repo.odb());
    index.set_cache_tree(Some(cache));
    Ok(index)
}

/// `read-tree --prefix=<dir>/ <tree>`: the tree's entries added below
/// `<dir>`; an entry that exists already is refused. Returns the index
/// and the paths that were added.
fn read_prefix(
    repo: &Repo,
    old: &Index,
    base: Option<&ObjectId>,
    tree: &ObjectId,
    dir: &str,
    paths: PathOptions,
) -> Result<(Index, Vec<Vec<u8>>), Die> {
    let mut dir = dir.as_bytes().to_vec();
    if !dir.is_empty() && !dir.ends_with(b"/") {
        dir.push(b'/');
    }
    let mut entries = entries_of(repo, tree, &dir, paths)?;
    if let Some(base) = base {
        let unchanged: std::collections::HashSet<(Vec<u8>, u32, ObjectId)> =
            entries_of(repo, base, &dir, paths)?
                .into_iter()
                .map(|e| (e.path().to_vec(), e.mode.0, e.id))
                .collect();
        entries.retain(|e| !unchanged.contains(&(e.path().to_vec(), e.mode.0, e.id)));
    }
    for e in &entries {
        if !old.path_range(e.path()).is_empty() {
            let shown = String::from_utf8_lossy(e.path()).into_owned();
            return Err(Die::Error(msg::cannot_bind(&shown), 128));
        }
    }
    let mut index = old.clone();
    let added: Vec<Vec<u8>> = entries.iter().map(|e| e.path().to_vec()).collect();
    for e in entries {
        bind(&mut index, e)?;
    }
    // A directory is valid where its tree exists already (the tree read
    // in, unchanged directories), the others are not.
    let cache = rgit_unpack::repair_cache_tree(&index, repo.odb());
    index.set_cache_tree(Some(cache));
    Ok((index, added))
}

/// `--prefix` with `-u`: writes the added entries into the work tree and
/// records their stat data.
fn check_out_new(
    repo: &Repo,
    index: &mut Index,
    added: &[Vec<u8>],
    mtime: Option<rgit_index::Time>,
) -> Result<(), Die> {
    let root = repo.work_tree()?;
    let checker = repo.checker(mtime)?;
    let conv = crate::convert::Converter::checkout(repo, &*index)?;
    let symlinks = checker.options().symlinks;
    for path in added {
        let Ok(pos) = index.position(path, rgit_index::Stage::MERGED) else {
            continue;
        };
        let entry = index.entries()[pos].clone();
        let data = if entry.mode.is_gitlink() {
            Vec::new()
        } else {
            let blob = rgit_unpack::read_blob(repo.odb(), &entry.id)
                .map_err(|e| Die::Error(e.to_string(), 128))?;
            if entry.mode.is_file() {
                conv.to_worktree(entry.path(), &entry.id, blob)
            } else {
                blob
            }
        };
        match rgit_unpack::write_entry(&root, path, &entry, &data, symlinks) {
            Ok(Some(meta)) => {
                if let Some(e) = index.entry_mut(pos) {
                    e.stat = rgit_worktree::stat::stat_from_metadata(&meta);
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("error: {e}"),
        }
    }
    Ok(())
}

/// `GIT_TEST_CHECK_CACHE_TREE`: verify the cache tree before writing the
/// index (git's test suite turns it on).
fn check_cache_tree() -> bool {
    std::env::var("GIT_TEST_CHECK_CACHE_TREE")
        .ok()
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_bytes())))
        .unwrap_or(false)
}

/// `read-tree [<tree>]` without `-m`: the index becomes the tree (or empty)
/// with a fully valid cache tree.
fn read_plain(
    repo: &Repo,
    old: &Index,
    tree: Option<&ObjectId>,
    paths: PathOptions,
) -> Result<Index, Die> {
    let mut index = old.clone();
    index.clear_entries();
    index.clear_resolve_undo();
    let Some(tree) = tree else {
        let root = TreeNode::new_valid(Vec::new(), 0, ObjectId::empty_tree(repo.hash));
        index.set_cache_tree(Some(CacheTree { root }));
        return Ok(index);
    };
    let entries = rgit_unpack::tree_entries(repo.odb(), tree).map_err(unpack_failed)?;
    for e in &entries {
        if !rgit_unpack::verify_checkout_path(e.path(), e.mode, paths) {
            return Err(unpack_failed(rgit_unpack::Error::InvalidPath(e.path().to_vec())));
        }
    }
    index.set_cache_tree(None);
    let add = AddOptions {
        skip_path_check: true,
        ..AddOptions::default()
    };
    let listed = entries.len();
    let untracked = index.take_untracked_cache();
    for e in entries {
        index.add(e, add).map_err(|err| Die::Fatal(err.to_string()))?;
    }
    index.set_untracked_cache(untracked);
    // git primes the cache tree from the tree object; a tree with duplicate
    // names then counts more entries than the index holds, which its cache
    // tree check (on in git's test suite) reports.
    if listed != index.len() && check_cache_tree() {
        eprintln!("error: {}", msg::CORRUPTED_CACHE_TREE);
        return Err(Die::Fatal(msg::UNABLE_TO_WRITE_INDEX.into()));
    }
    let tree = rgit_unpack::repair_cache_tree(&index, repo.odb());
    index.set_cache_tree(Some(tree));
    Ok(index)
}
