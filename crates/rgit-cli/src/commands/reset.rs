// SPDX-License-Identifier: MIT
//! `git reset` (Documentation/git-reset.adoc).
//!
//! Implemented:
//! - `--soft [<commit>]`: moves `HEAD`, records `ORIG_HEAD` and the reflog
//!   entry `reset: moving to <commit>`.
//! - `--mixed [<commit>]` (the default): also rebuilds the index from the
//!   commit's tree. Entries whose mode and object id do not change keep
//!   their cached stat data; then the index is refreshed against the work
//!   tree and the paths that still differ are listed under "Unstaged
//!   changes after reset:" (unless `-q` or `--no-refresh`). `-N` keeps
//!   paths that are missing from the tree as intent-to-add entries.
//! - `[<tree-ish>] [--] <pathspec>...` and `--pathspec-from-file`: the same
//!   for the matching paths only; `HEAD` does not move.
//!
//! - `--hard [<commit>]`: also makes the work tree match (one-way reset
//!   through `rgit-unpack`) and prints "HEAD is now at ...".
//! - `--merge` and `--keep` as in the DISCUSSION tables of git-reset(1):
//!   a one-way merge that requires changed entries to be up to date, and a
//!   two-way merge from `HEAD` whose index then becomes the target.
//!
//! Not yet: `--merge` with unmerged entries (needs three-way merges,
//! #158), `--patch` (#109), `--recurse-submodules` (#121).

use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Index, Mode as IndexMode, Stage, WriteOptions};
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_refs::{RefUpdate, Transaction};
use rgit_worktree::stat::{Status, smudge_racy_entries};

use crate::messages::reset as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repo::Mode;
use crate::repository::{self, Repo};

const MIXED: i32 = 1;
const SOFT: i32 = 2;
const HARD: i32 = 3;
const MERGE: i32 = 4;
const KEEP: i32 = 5;

fn mode_name(mode: i32) -> &'static str {
    match mode {
        SOFT => "soft",
        HARD => "hard",
        MERGE => "merge",
        KEEP => "keep",
        _ => "mixed",
    }
}

#[cfg(unix)]
fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    root.join(std::ffi::OsStr::from_bytes(rel))
}

#[cfg(not(unix))]
fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    root.join(String::from_utf8_lossy(rel).as_ref())
}

/// A path given on the command line, relative to the directory the
/// command was started in; the process runs at the top of the work tree.
fn from_start_dir(repo: &Repo, arg: &str) -> PathBuf {
    match &repo.setup.prefix {
        Some(p) if !Path::new(arg).is_absolute() => Path::new(p).join(arg),
        _ => PathBuf::from(arg),
    }
}

/// Whether `arg` can stand for files in the work tree (git's check before
/// it takes an argument as a path without `--`).
fn looks_like_file(repo: &Repo, arg: &str) -> bool {
    if arg.starts_with(':') && arg != ":" && !arg.starts_with(":/") {
        return true;
    }
    if arg.starts_with(":/") {
        return true;
    }
    if arg.bytes().any(|b| matches!(b, b'*' | b'?' | b'[' | b'\\')) {
        return true;
    }
    std::fs::symlink_metadata(from_start_dir(repo, arg)).is_ok()
}

/// The `tree` of a commit (or the tree itself), after peeling tags.
fn tree_of(objects: &Objects, id: &ObjectId) -> Option<ObjectId> {
    let id = objects.peel(id)?;
    match objects.read(&id)? {
        (ObjectKind::Tree, _) => Some(id),
        (ObjectKind::Commit, data) => {
            let line = data.split(|&b| b == b'\n').next()?;
            let hex = std::str::from_utf8(line.strip_prefix(b"tree ")?).ok()?;
            ObjectId::from_hex(objects.hash(), hex.as_bytes()).ok()
        }
        _ => None,
    }
}

/// The blobs (and gitlinks) of `tree`, with full paths, in tree order.
fn flatten(
    repo: &Repo,
    tree: &ObjectId,
    base: &[u8],
    out: &mut Vec<(Vec<u8>, u32, ObjectId)>,
) -> Result<(), Die> {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return Err(Die::Fatal(msg::failed_to_resolve_tree(&tree.to_hex())));
    };
    for e in t.entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical();
        if mode.is_tree() {
            path.push(b'/');
            flatten(repo, &e.oid, &path, out)?;
        } else {
            out.push((path, mode.bits(), e.oid));
        }
    }
    Ok(())
}

/// Makes the index entries selected by `wanted` match `tree` (`None`: the
/// empty tree). Unchanged entries keep their stat data and flags; with
/// `intent_to_add`, selected paths missing from the tree become
/// intent-to-add entries instead of being removed.
fn reset_index(
    repo: &Repo,
    index: &mut Index,
    tree: Option<&ObjectId>,
    wanted: &dyn Fn(&[u8]) -> bool,
    intent_to_add: bool,
) -> Result<(), Die> {
    let mut target = Vec::new();
    if let Some(tree) = tree {
        flatten(repo, tree, b"", &mut target)?;
    }
    target.retain(|(p, _, _)| wanted(p));
    target.sort_by(|a, b| a.0.cmp(&b.0));

    // Paths in the index that the tree does not have.
    let mut gone: Vec<(Vec<u8>, IndexMode, bool)> = Vec::new();
    let mut last: Option<&[u8]> = None;
    for e in index.entries() {
        if last == Some(e.path()) || !wanted(e.path()) {
            continue;
        }
        last = Some(e.path());
        if target
            .binary_search_by(|t| t.0.as_slice().cmp(e.path()))
            .is_err()
        {
            let ita = e.stage() == Stage::MERGED && e.flags.intent_to_add;
            gone.push((e.path().to_vec(), e.mode, ita));
        }
    }
    let empty_blob = repo.hash.empty_blob();
    if intent_to_add && !gone.is_empty() {
        repo.odb()
            .write(ObjectKind::Blob, b"")
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    for (path, mode, ita) in gone {
        if intent_to_add {
            if ita {
                continue;
            }
            index.remove_path(&path);
            let mut entry = Entry::new(path, Stage::MERGED, mode, empty_blob);
            entry.flags.intent_to_add = true;
            index
                .add(entry, AddOptions::default())
                .map_err(|e| Die::Fatal(e.to_string()))?;
        } else {
            index.remove_path(&path);
        }
    }

    for (path, mode, id) in target {
        let same = {
            let entries = index.entries_for_path(&path);
            entries.len() == 1
                && entries[0].stage() == Stage::MERGED
                && entries[0].mode.0 == mode
                && entries[0].id == id
                && !entries[0].flags.intent_to_add
        };
        if same {
            continue;
        }
        index.remove_path(&path);
        let entry = Entry::new(path, Stage::MERGED, IndexMode(mode), id);
        index
            .add(
                entry,
                AddOptions {
                    ok_to_replace: true,
                    ..Default::default()
                },
            )
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    Ok(())
}

/// Refreshes the stat data of `index` and returns the "Unstaged changes"
/// lines (`<letter>\t<path>`).
fn refresh(
    repo: &Repo,
    index: &mut Index,
    checker: &rgit_worktree::stat::Checker,
    work_tree: &Path,
) -> Result<Vec<u8>, Die> {
    let mut out = Vec::new();
    let mut line = |letter: u8, path: &[u8]| {
        out.push(letter);
        out.push(b'\t');
        out.extend_from_slice(path);
        out.push(b'\n');
    };
    let mut last_unmerged: Option<Vec<u8>> = None;
    for pos in 0..index.len() {
        let e = &index.entries()[pos];
        if e.stage().is_conflict() {
            if last_unmerged.as_deref() != Some(e.path()) {
                line(b'U', e.path());
                last_unmerged = Some(e.path().to_vec());
            }
            continue;
        }
        if e.flags.skip_worktree || e.flags.assume_valid {
            continue;
        }
        if e.flags.intent_to_add {
            line(b'A', e.path());
            continue;
        }
        if e.mode.is_gitlink() {
            let dir = os_path(work_tree, e.path());
            if repository::submodule_head(&dir, repo.hash).is_some_and(|h| h != e.id) {
                line(b'M', e.path());
            }
            continue;
        }
        let status = checker
            .check(e)
            .map_err(|err| Die::Fatal(format!("{}: {err}", String::from_utf8_lossy(e.path()))))?;
        match status {
            Status::Unchanged => {}
            Status::StatOnly => {
                if let Ok(Some(meta)) = checker.lstat(e.path()) {
                    let entry = index.entry_mut(pos).expect("entry");
                    checker.refresh(entry, &meta);
                }
            }
            Status::Modified => line(b'M', index.entries()[pos].path()),
            Status::TypeChanged => line(b'T', index.entries()[pos].path()),
            Status::Deleted => line(b'D', index.entries()[pos].path()),
        }
    }
    Ok(out)
}

/// How the arguments split into a revision and pathspecs.
struct Args {
    rev: Option<String>,
    paths: Vec<String>,
    /// The revision came with `--` right after it.
    dashdash: bool,
}

fn split_args(repo: &Repo, revs: &Revs, args: &[String]) -> Result<Args, Die> {
    let Some(first) = args.first() else {
        return Ok(Args {
            rev: None,
            paths: Vec::new(),
            dashdash: false,
        });
    };
    if first == "--" {
        return Ok(Args {
            rev: None,
            paths: args[1..].to_vec(),
            dashdash: false,
        });
    }
    if args.get(1).is_some_and(|a| a == "--") {
        return Ok(Args {
            rev: Some(first.clone()),
            paths: args[2..].to_vec(),
            dashdash: true,
        });
    }
    if revs.resolve(first).is_some() {
        if looks_like_file(repo, first) && !first.starts_with(':') {
            return Err(Die::Fatal(msg::both_revision_and_filename(first)));
        }
        return Ok(Args {
            rev: Some(first.clone()),
            paths: args[1..].to_vec(),
            dashdash: false,
        });
    }
    if !looks_like_file(repo, first) {
        return Err(Die::Fatal(msg::ambiguous(first)));
    }
    Ok(Args {
        rev: None,
        paths: args.to_vec(),
        dashdash: false,
    })
}

fn write_index(repo: &Repo, index: &Index) -> Result<(), Die> {
    let path = repo.setup.cwd.join(repo.index_path());
    let skip_hash = repo
        .config
        .bool("index.skiphash")
        .ok()
        .flatten()
        .unwrap_or(false);
    match index.write_file(
        &path,
        &rgit_fs::LockOptions::default(),
        &WriteOptions { skip_hash },
    ) {
        Ok(()) => Ok(()),
        Err(rgit_index::WriteError::Lock(e)) => {
            if let Some(w) = e.warning() {
                eprintln!("warning: {w}");
            }
            Err(Die::Fatal(e.to_string()))
        }
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(0);
    let no_refresh = Cell::new(false);
    let mode = Cell::new(0);
    let recurse = RefCell::new(None);
    let patch = Cell::new(false);
    let auto_advance = Cell::new(false);
    let unified = Cell::new(-1);
    let inter_hunk = Cell::new(-1);
    let intent_to_add = Cell::new(false);
    let from_file = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::flag(None, "no-refresh", &no_refresh, msg::HELP_NO_REFRESH),
            Opt::set_int(None, "mixed", &mode, msg::HELP_MIXED, MIXED).noneg(),
            Opt::set_int(None, "soft", &mode, msg::HELP_SOFT, SOFT).noneg(),
            Opt::set_int(None, "hard", &mode, msg::HELP_HARD, HARD).noneg(),
            Opt::set_int(None, "merge", &mode, msg::HELP_MERGE, MERGE).noneg(),
            Opt::set_int(None, "keep", &mode, msg::HELP_KEEP, KEEP).noneg(),
            Opt::string(
                None,
                "recurse-submodules",
                &recurse,
                "reset",
                msg::HELP_RECURSE_SUBMODULES,
            )
            .optional(Some("")),
            Opt::flag('p', "patch", &patch, msg::HELP_PATCH),
            Opt::flag(None, "auto-advance", &auto_advance, msg::HELP_AUTO_ADVANCE),
            Opt::integer('U', "unified", &unified, msg::HELP_UNIFIED).noneg(),
            Opt::integer(None, "inter-hunk-context", &inter_hunk, msg::HELP_INTER_HUNK).noneg(),
            Opt::flag('N', "intent-to-add", &intent_to_add, msg::HELP_INTENT_TO_ADD),
            Opt::string(
                None,
                "pathspec-from-file",
                &from_file,
                "file",
                msg::HELP_PATHSPEC_FROM_FILE,
            ),
            Opt::flag(None, "pathspec-file-nul", &file_nul, msg::HELP_PATHSPEC_FILE_NUL),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    drop(parser);
    let quiet = quiet.get() > 0;
    let mode = mode.get();
    let from_file = from_file.borrow().clone();

    if patch.get() {
        if from_file.is_some() {
            return Err(Die::Fatal(msg::FROM_FILE_WITH_PATCH.to_owned()));
        }
        if mode != 0 {
            return Err(Die::Fatal(msg::PATCH_WITH_MODE.to_owned()));
        }
        return Err(Die::Fatal(msg::not_supported("--patch")));
    }
    if file_nul.get() && from_file.is_none() {
        return Err(Die::Fatal(msg::FILE_NUL_WITHOUT_FROM_FILE.to_owned()));
    }
    if recurse.borrow().is_some() {
        return Err(Die::Fatal(msg::not_supported("--recurse-submodules")));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let revs = Revs::new(ctx, &repo.setup)?;
    let mut split = split_args(&repo, &revs, &args)?;

    if let Some(file) = &from_file {
        if !split.paths.is_empty() {
            return Err(Die::Fatal(msg::FROM_FILE_WITH_ARGS.to_owned()));
        }
        let data = if file == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
                .map_err(|e| Die::Fatal(msg::could_not_open(file, &crate::global::strerror(&e))))?;
            buf
        } else {
            let path = from_start_dir(&repo, file);
            std::fs::read(&path).map_err(|e| {
                Die::Fatal(msg::could_not_open(
                    &path.to_string_lossy(),
                    &crate::global::strerror(&e),
                ))
            })?
        };
        let items = rgit_pathspec::parse_pathspec_file(&data, file_nul.get())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        split.paths = items
            .into_iter()
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect();
        if split.paths.is_empty() {
            // An empty file selects nothing, like no pathspec at all.
            split.paths = Vec::new();
        }
    }
    let has_paths = !split.paths.is_empty() || from_file.is_some();

    let objects = Objects::open(&repo.setup);
    let head = revs.resolve("HEAD");
    let rev_name = split.rev.clone().unwrap_or_else(|| "HEAD".to_owned());
    let unborn_head = head.is_none() && rev_name == "HEAD" && (split.rev.is_none() || split.dashdash);

    // The commit (whole reset) or tree (paths) to reset to; `None` for the
    // empty tree of an unborn branch.
    let mut commit = None;
    let tree;
    if has_paths {
        tree = if unborn_head {
            None
        } else {
            let id = revs
                .resolve(&rev_name)
                .ok_or_else(|| Die::Fatal(msg::failed_to_resolve_tree(&rev_name)))?;
            Some(tree_of(&objects, &id).ok_or_else(|| Die::Fatal(msg::not_a_commit(&rev_name)))?)
        };
    } else if unborn_head {
        tree = None;
    } else {
        let Some(target) = revs.resolve(&rev_name) else {
            return Err(Die::Fatal(msg::failed_to_resolve(&rev_name)));
        };
        let peeled = objects.peel(&target);
        let kind = peeled.as_ref().and_then(|id| objects.kind(id));
        let id = match (peeled, kind) {
            (Some(id), Some(ObjectKind::Commit)) => id,
            (Some(id), Some(k)) => {
                eprintln!("{}", msg::wrong_kind(&id.to_string(), &k.to_string()));
                return Err(Die::Fatal(msg::not_a_commit(&rev_name)));
            }
            _ => return Err(Die::Fatal(msg::not_a_commit(&rev_name))),
        };
        commit = Some(id);
        tree = tree_of(&objects, &id);
    }

    let explicit_mixed = mode == MIXED;
    let mode = if mode == 0 { MIXED } else { mode };
    if intent_to_add.get() && mode != MIXED {
        return Err(Die::Fatal(msg::N_REQUIRES_MIXED.to_owned()));
    }
    if has_paths {
        if mode != MIXED {
            return Err(Die::Fatal(msg::with_paths(mode_name(mode))));
        }
        if explicit_mixed {
            eprintln!("warning: {}", msg::MIXED_WITH_PATHS);
        }
    }
    if mode != SOFT && repo.setup.work_tree.is_none() {
        return Err(Die::Fatal(msg::bare(mode_name(mode))));
    }
    if mode == SOFT && repo.setup.git_dir.join("MERGE_HEAD").exists() {
        return Err(Die::Fatal(msg::SOFT_IN_MERGE.to_owned()));
    }
    if matches!(mode, HARD | MERGE | KEEP) {
        let empty = ObjectId::empty_tree(repo.hash);
        let head_tree = head.and_then(|h| tree_of(&objects, &h)).unwrap_or(empty);
        reset_worktree(&repo, mode, head_tree, tree.unwrap_or(empty), &rev_name)?;
    }

    if mode == MIXED {
        let pathspec = if has_paths {
            let work_tree = repo.work_tree()?;
            let opts = ParseOptions {
                prefix: repo.prefix(),
                root: Some(work_tree.as_os_str().as_encoded_bytes().to_vec()),
                globals: repository::pathspec_globals(ctx),
                ..Default::default()
            };
            Some(Pathspec::parse(&split.paths, &opts).map_err(|e| Die::Fatal(e.to_string()))?)
        } else {
            None
        };
        let wanted = |p: &[u8]| match &pathspec {
            Some(ps) => !ps.is_empty() && ps.match_path(p).is_some(),
            None => true,
        };
        let (mut index, index_mtime) = repo.read_index()?;
        reset_index(&repo, &mut index, tree.as_ref(), &wanted, intent_to_add.get())?;
        let work_tree = repo.work_tree()?;
        let checker = repo.checker(index_mtime)?;
        if !no_refresh.get() {
            let lines = refresh(&repo, &mut index, &checker, &work_tree)?;
            if !quiet && !lines.is_empty() {
                use std::io::Write;
                let mut o = std::io::stdout().lock();
                let _ = writeln!(o, "{}", msg::UNSTAGED_HEADER);
                let _ = o.write_all(&lines);
                let _ = o.flush();
            }
        }
        if let Some(reference) = index_mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        write_index(&repo, &index)?;
    }

    if has_paths {
        return Ok(0);
    }
    let Some(commit) = commit else {
        remove_branch_state(&repo.setup.git_dir, repo.hash);
        return Ok(0);
    };
    let config = refs_support::load_config(ctx, &repo.setup)?;
    let store = refs_support::ref_store(&repo.setup, &config);
    let mut hooks = UpdateHooks::new(&repo.setup, &config, &objects);
    if let Some(old) = head {
        let update = RefUpdate::update("ORIG_HEAD", old, None)
            .no_deref(true)
            .message(msg::UPDATING_ORIG_HEAD);
        let mut tx = Transaction::new(&store);
        tx.push(update)
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| Die::Error(e.to_string(), 1))?;
    }
    let update = RefUpdate::update("HEAD", commit, head).message(msg::moving_to(&rev_name));
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Error(e.to_string(), 1))?;
    remove_branch_state(&repo.setup.git_dir, repo.hash);
    if mode == HARD && !quiet {
        println!(
            "{}",
            crate::messages::switch::head_now_at(
                &objects.abbrev(&commit, 7),
                &crate::switch_branch::subject(&objects, &commit)
            )
        );
    }
    Ok(0)
}

/// `--hard`, `--merge`, `--keep`: moves the index and the work tree from
/// `head` to `target` (trees) as the DISCUSSION tables of git-reset(1)
/// describe. `--hard` is a one-way reset; `--merge` a one-way merge that
/// requires changed entries to be up to date; `--keep` a two-way merge
/// from `HEAD` whose index then becomes `target` (local changes stay in
/// the work tree).
fn reset_worktree(
    repo: &Repo,
    mode: i32,
    head: ObjectId,
    target: ObjectId,
    rev: &str,
) -> Result<(), Die> {
    use crate::commands::read_tree::{lock_failed, path_options, unpack_failed};
    use rgit_fs::{LockFile, LockOptions};
    use rgit_unpack::{Identity, Merge, Options, Worktree};

    let work_tree = repo.work_tree()?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    let (index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    // No refresh: like git, a touched file counts as not up to date.
    if mode == MERGE && index.has_conflicts() {
        return Err(Die::Fatal(msg::not_supported("--merge with unmerged entries")));
    }
    let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
    let wt = Worktree {
        root: &work_tree,
        checker: &checker,
    };
    let merge = match mode {
        KEEP => Merge::TwoWay {
            old: head,
            new: target,
        },
        _ => Merge::OneWay(target),
    };
    let mut opts = Options {
        reset: mode == HARD,
        update: true,
        paths: path_options(repo),
        excludes: Some(&mut excludes),
        ..Options::default()
    };
    let plan = match rgit_unpack::plan(repo.odb(), &index, &merge, Some(&wt), &mut opts) {
        Ok(p) => p,
        Err(rgit_unpack::Error::Rejected(r)) => {
            for m in r.plumbing_messages() {
                eprintln!("error: {m}");
            }
            return Err(Die::Fatal(msg::could_not_reset_index(rev)));
        }
        Err(rgit_unpack::Error::Unmerged) => {
            return Err(Die::Fatal(msg::could_not_reset_index(rev)));
        }
        Err(e) => return Err(unpack_failed(e)),
    };
    let applied = plan.apply(repo.odb(), &wt, &Identity);
    let mut index = applied.index;
    if mode == KEEP {
        reset_index(repo, &mut index, Some(&target), &|_| true, false)?;
    }
    if let Some(reference) = mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock)?;
    if !applied.errors.is_empty() {
        for e in &applied.errors {
            eprintln!("error: {e}");
        }
        return Err(Die::Exit(128));
    }
    Ok(())
}

/// Forgets an operation in progress after a whole reset: the merge state
/// files, and a cherry-pick or revert (with its sequencer directory).
fn remove_branch_state(git_dir: &Path, hash: rgit_hash::HashKind) {
    let holds_oid = |name: &str| {
        std::fs::read_to_string(git_dir.join(name))
            .ok()
            .is_some_and(|s| ObjectId::from_hex(hash, s.trim_end().as_bytes()).is_ok())
    };
    let mut picking = false;
    for name in ["CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        if holds_oid(name) {
            picking = true;
            let _ = std::fs::remove_file(git_dir.join(name));
        }
    }
    if picking {
        let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
    }
    for name in ["MERGE_HEAD", "MERGE_RR", "MERGE_MSG", "MERGE_MODE", "SQUASH_MSG"] {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
    if holds_oid("AUTO_MERGE") {
        let _ = std::fs::remove_file(git_dir.join("AUTO_MERGE"));
    }
}
