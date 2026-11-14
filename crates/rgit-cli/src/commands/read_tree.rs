// SPDX-License-Identifier: MIT
//! `git read-tree` (Documentation/git-read-tree.adoc): `read-tree <tree-ish>`
//! (replaces the index and primes the cache tree), `--empty`, and the one-
//! and two-way merges `-m`/`--reset` with `-u`, `-i`, `-n` and
//! `--index-output` through the unpack engine (`rgit-unpack`, #107).
//! Not implemented yet: three-way merges (#158), `--prefix`, and several
//! trees without `-m`.

use std::path::{Path, PathBuf};

use rgit_fs::{LockError, LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_index::{AddOptions, CacheTree, Index, TreeNode, WriteOptions};
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
            Opt::callback(None, "recurse-submodules", Some("checkout"), msg::HELP_RECURSE, |_, _| {
                Ok(())
            })
            .optional(None),
            Opt::quiet(&quiet, msg::HELP_QUIET),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let _ = (trivial.get(), aggressive.get(), verbose.get(), quiet.get());

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
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
    if mode == MODE_PREFIX {
        return Err(Die::Fatal(msg::PREFIX_UNSUPPORTED.into()));
    }
    if trees.len() > 2 {
        return Err(Die::Fatal(msg::THREE_WAY_UNSUPPORTED.into()));
    }
    if mode == MODE_NONE && trees.len() > 1 {
        return Err(Die::Fatal(msg::OVERLAY_UNSUPPORTED.into()));
    }

    let index_path = repo.setup.cwd.join(repo.index_path());
    let mut lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    let (old, old_mtime) = repo.read_index()?;
    let paths = path_options(&repo);

    let index = if mode == MODE_NONE {
        read_plain(&repo, &old, trees.first(), paths)?
    } else {
        let merge = match trees[..] {
            [t] => Merge::OneWay(t),
            [old, new] => Merge::TwoWay { old, new },
            _ => unreachable!("checked above"),
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
        let mut opts = Options {
            reset: mode == MODE_RESET,
            discard_unmerged: false,
            update,
            index_only,
            show_all_errors: false,
            initial_checkout: old.is_empty() && old_mtime.is_none(),
            paths,
            excludes: excludes.as_mut(),
        };
        let wt = match (&root, &checker) {
            (Some(root), Some(checker)) => Some(Worktree { root, checker }),
            _ => None,
        };
        let plan = rgit_unpack::plan(repo.odb(), &old, &merge, wt.as_ref(), &mut opts)
            .map_err(unpack_failed)?;
        if dry_run.get() != 0 {
            return Ok(0);
        }
        match (&wt, update) {
            (Some(wt), true) => {
                let conv = crate::convert::Converter::checkout(&repo, plan.index())?;
                let applied = plan.apply(repo.odb(), wt, &conv);
                if conv.skipped_filter() {
                    eprintln!("warning: {}", msg::ATTRIBUTES_NOT_APPLIED);
                }
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
    let skip_hash = repo
        .config
        .bool("index.skiphash")
        .ok()
        .flatten()
        .unwrap_or(false);
    let opts = WriteOptions { skip_hash };
    let fail = || Die::Fatal(msg::UNABLE_TO_WRITE_INDEX.into());
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

/// `read-tree [<tree>]` without `-m`: the index becomes the tree (or empty)
/// with a fully valid cache tree.
fn read_plain(
    repo: &Repo,
    old: &Index,
    tree: Option<&ObjectId>,
    paths: PathOptions,
) -> Result<Index, Die> {
    let mut index = old.clone();
    index.retain(|_| false);
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
    for e in entries {
        index.add(e, add).map_err(|err| Die::Fatal(err.to_string()))?;
    }
    let tree = rgit_unpack::repair_cache_tree(&index, repo.odb());
    index.set_cache_tree(Some(tree));
    Ok(index)
}
