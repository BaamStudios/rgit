// SPDX-License-Identifier: MIT
//! `git restore` (Documentation/git-restore.adoc), and the path checkout
//! it shares with `git checkout [<tree-ish>] [--] <pathspec>...`
//! (git-checkout.adoc).
//!
//! Implemented: `-s`/`--source` (also `<a>...<b>`), `-S`/`--staged`,
//! `-W`/`--worktree`, `--overlay`/`--no-overlay`, `--ours`/`--theirs`,
//! `--ignore-unmerged`, `--ignore-skip-worktree-bits`, `-q`,
//! `--pathspec-from-file`/`--pathspec-file-nul`.
//!
//! Not yet: `-m`/`--merge`, `--conflict` (they need the three-way file
//! merge), `-p`, `--recurse-submodules`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Stage};
use rgit_object::ObjectKind;
use rgit_pathspec::{GlobalFlags, ParseOptions, Pathspec};
use rgit_worktree::stat::{Status, smudge_racy_entries};

use crate::commands::read_tree::{lock_failed, unpack_failed};
use crate::messages::reset as rmsg;
use crate::messages::restore as msg;
use crate::messages::switch as smsg;
use crate::prelude::*;
use crate::refs_support::{Objects, Revs};
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::switch_branch as sb;

/// What a path checkout reads and what it updates.
pub(crate) struct PathCheckout {
    /// The tree to take the content from; `None`: the index.
    pub source: Option<ObjectId>,
    /// Update the files in the work tree.
    pub worktree: bool,
    /// Update the index entries from `source`.
    pub index: bool,
    /// Overlay mode: never remove files (no-overlay removes tracked paths
    /// the source does not have).
    pub overlay: bool,
    /// `--ours`/`--theirs`: the stage to write for unmerged paths.
    pub stage: Option<Stage>,
    /// Leave unmerged paths alone with a warning (`-f`,
    /// `--ignore-unmerged`) instead of failing.
    pub ignore_unmerged: bool,
    /// Also restore entries outside the sparse checkout.
    pub ignore_skip_worktree: bool,
    /// Print "Updated N paths from ..." (on standard error).
    pub report: bool,
    /// No warnings about unmerged paths left alone.
    pub quiet: bool,
}

/// What happens to one path.
enum Action {
    /// Write this entry's content to the work tree.
    Write(Entry),
    /// Remove the path (`gitlink`: a submodule directory).
    Remove { gitlink: bool },
    /// Remove the path from the index only (an unmerged path left alone).
    Unindex,
}

/// `--conflict=<style>`: "error: unknown conflict style" for anything
/// but the three styles git knows.
pub(crate) fn check_conflict_style(style: Option<&str>) -> Result<(), Die> {
    match style {
        None | Some("merge" | "diff3" | "zdiff3") => Ok(()),
        Some(other) => Err(Die::Error(msg::unknown_conflict_style(other), 129)),
    }
}

/// Reads the `--pathspec-from-file` input (`name`, relative to the
/// directory the command was started in; `-` is standard input).
pub(crate) fn read_pathspec_file(repo: &Repo, name: &str, nul: bool) -> Result<Vec<Vec<u8>>, Die> {
    use std::io::Read;
    let mut data = Vec::new();
    let read = if name == "-" {
        std::io::stdin().read_to_end(&mut data).map(|_| ())
    } else {
        let full = match &repo.setup.prefix {
            Some(p) if !Path::new(name).is_absolute() => Path::new(p).join(name),
            _ => PathBuf::from(name),
        };
        std::fs::File::open(full).and_then(|mut f| f.read_to_end(&mut data).map(|_| ()))
    };
    read.map_err(|e| Die::Fatal(rmsg::could_not_open(name, &crate::global::strerror(&e))))?;
    rgit_pathspec::parse_pathspec_file(&data, nul).map_err(|e| Die::Fatal(e.to_string()))
}

/// The tree of the tree-ish `id`.
pub(crate) fn tree_of(repo: &Repo, objects: &Objects, id: &ObjectId) -> Option<ObjectId> {
    let p = objects.peel(id)?;
    match objects.kind(&p)? {
        ObjectKind::Tree => Some(p),
        ObjectKind::Commit => sb::commit_tree(repo, &p),
        _ => None,
    }
}

/// Restores the paths matching `specs` as `how` says. Unmatched
/// pathspecs and unmerged paths that cannot be restored are errors
/// (exit 1) before anything changes.
#[allow(clippy::too_many_lines)]
pub(crate) fn checkout_paths<S: AsRef<[u8]>>(
    globals: GlobalFlags,
    repo: &Repo,
    objects: &Objects,
    specs: &[S],
    how: &PathCheckout,
) -> CmdResult {
    let work_tree = repo.work_tree()?;
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root: Some(work_tree.as_os_str().as_encoded_bytes().to_vec()),
        globals,
        ..Default::default()
    };
    let ps = Pathspec::parse(specs, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    let (mut index, mtime) = repo.read_index()?;
    // No refresh: like git, files whose stat data changed are rewritten
    // (and counted) even when their content is the same.
    let checker = repo.checker(mtime)?;
    let mut seen = vec![false; ps.len()];
    let mut actions: Vec<(Vec<u8>, Action)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut index_changed = false;
    // An unmerged path left alone or failing.
    let unmerged = |path: &[u8], errors: &mut Vec<String>| {
        let p = String::from_utf8_lossy(path);
        if how.ignore_unmerged {
            if !how.quiet {
                eprintln!("warning: {}", smsg::path_unmerged(&p));
            }
        } else {
            errors.push(smsg::path_unmerged(&p));
        }
    };

    match how.source {
        Some(t) => {
            let original = index.entries().to_vec();
            let entries = rgit_unpack::tree_entries(repo.odb(), &t).map_err(unpack_failed)?;
            let mut in_tree: HashSet<Vec<u8>> = HashSet::new();
            for e in entries {
                if ps.match_path_seen(e.path(), &mut seen).is_none() {
                    continue;
                }
                in_tree.insert(e.path().to_vec());
                let cur = index.entries_for_path(e.path());
                let same = cur.len() == 1
                    && cur[0].stage() == Stage::MERGED
                    && cur[0].mode == e.mode
                    && cur[0].id == e.id
                    && !cur[0].flags.intent_to_add;
                let clean = same && matches!(checker.check(&cur[0]), Ok(Status::Unchanged));
                if how.index && !same {
                    index.remove_path(e.path());
                    index
                        .add(
                            e.clone(),
                            AddOptions {
                                ok_to_replace: true,
                                ..Default::default()
                            },
                        )
                        .map_err(|err| Die::Fatal(err.to_string()))?;
                    index_changed = true;
                }
                if !clean {
                    actions.push((e.path().to_vec(), Action::Write(e)));
                }
            }
            if !how.overlay {
                // Tracked paths the source does not have.
                let mut last: Option<&[u8]> = None;
                for e in &original {
                    if ps.match_path_seen(e.path(), &mut seen).is_none()
                        || in_tree.contains(e.path())
                        || last == Some(e.path())
                    {
                        continue;
                    }
                    last = Some(e.path());
                    if e.stage().is_conflict() {
                        // Left alone in the work tree, but gone from the
                        // index like the paths that are not unmerged.
                        unmerged(e.path(), &mut errors);
                        actions.push((e.path().to_vec(), Action::Unindex));
                        continue;
                    }
                    actions.push((
                        e.path().to_vec(),
                        Action::Remove {
                            gitlink: e.mode.is_gitlink(),
                        },
                    ));
                }
            }
        }
        None => {
            let mut last: Option<&[u8]> = None;
            for e in index.entries() {
                if ps.match_path_seen(e.path(), &mut seen).is_none() {
                    continue;
                }
                if e.stage().is_conflict() {
                    // The first stage stands for the path.
                    if last == Some(e.path()) {
                        continue;
                    }
                    last = Some(e.path());
                    let all = index.entries_for_path(e.path());
                    let Some(stage) = how.stage else {
                        unmerged(e.path(), &mut errors);
                        continue;
                    };
                    match all.iter().find(|x| x.stage() == stage) {
                        Some(chosen) => actions.push((e.path().to_vec(), Action::Write(chosen.clone()))),
                        None if !how.overlay => actions.push((
                            e.path().to_vec(),
                            Action::Remove {
                                gitlink: e.mode.is_gitlink(),
                            },
                        )),
                        None => {
                            let side = if stage == Stage::OURS { "our" } else { "their" };
                            errors.push(msg::no_version(&String::from_utf8_lossy(e.path()), side));
                        }
                    }
                    continue;
                }
                if e.flags.intent_to_add || (e.flags.skip_worktree && !how.ignore_skip_worktree) {
                    continue;
                }
                if !matches!(checker.check(e), Ok(Status::Unchanged)) {
                    actions.push((e.path().to_vec(), Action::Write(e.clone())));
                }
            }
        }
    }
    if report_unmatched(&ps, &seen) {
        return Err(Die::Exit(1));
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("error: {e}");
        }
        return Err(Die::Exit(1));
    }

    let symlinks = checker.options().symlinks;
    let mut failed = false;
    let mut count = 0usize;
    // Removals first: a removed file may stand where a directory goes.
    for (path, action) in &actions {
        let gitlink = match action {
            Action::Remove { gitlink } => gitlink,
            Action::Unindex => {
                if how.index {
                    index.remove_path(path);
                    index_changed = true;
                }
                continue;
            }
            Action::Write(_) => continue,
        };
        if how.worktree {
            if let Err(e) = rgit_unpack::remove_entry(&work_tree, path, *gitlink) {
                eprintln!(
                    "error: {}",
                    rgit_unpack::messages::unable_to_unlink(&String::from_utf8_lossy(path), &crate::global::strerror(&e))
                );
                failed = true;
            }
        }
        if how.index && how.source.is_some() {
            index.remove_path(path);
            index_changed = true;
        }
    }
    let conv = crate::convert::Converter::paths(repo, &index)?;
    for (path, action) in &actions {
        let Action::Write(entry) = action else {
            continue;
        };
        count += 1;
        if !how.worktree {
            continue;
        }
        let data = if entry.mode.is_gitlink() {
            Vec::new()
        } else {
            match rgit_unpack::read_blob(repo.odb(), &entry.id) {
                Ok(d) if entry.mode.is_file() => conv.to_worktree(path, &entry.id, d),
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: {e}");
                    failed = true;
                    continue;
                }
            }
        };
        match rgit_unpack::write_entry(&work_tree, path, entry, &data, symlinks) {
            Ok(Some(meta)) => {
                // The index entry (for `--ours`/`--theirs` the stage
                // written) now describes the new file, unless the index
                // keeps something else (worktree-only restore from a tree).
                let stage = if how.source.is_some() { Stage::MERGED } else { entry.stage() };
                if how.source.is_none() || how.index {
                    if let Ok(pos) = index.position(path, stage) {
                        if let Some(e) = index
                            .entry_mut(pos)
                            .filter(|e| e.id == entry.id && e.mode == entry.mode)
                        {
                            e.stat = rgit_worktree::stat::stat_from_metadata(&meta);
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("error: {e}");
                failed = true;
            }
        }
    }
    if index_changed {
        index.set_cache_tree(None);
        let ct = rgit_unpack::repair_cache_tree(&index, repo.odb());
        index.set_cache_tree(Some(ct));
    }
    if how.source.is_none() || how.index {
        if let Some(reference) = mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        sb::write_index(repo, &index, lock)?;
    } else {
        drop(lock);
    }
    if how.report {
        match how.source {
            Some(t) => eprintln!("{}", smsg::updated_paths_from(count, &objects.abbrev(&t, 7))),
            None => eprintln!("{}", smsg::updated_paths_from_index(count)),
        }
    }
    Ok(if failed { 1 } else { 0 })
}

/// Prints "pathspec ... did not match" for every unmatched item; true if
/// there was one.
fn report_unmatched(ps: &Pathspec, seen: &[bool]) -> bool {
    let mut any = false;
    for item in ps.unmatched(seen) {
        eprintln!(
            "error: {}",
            smsg::pathspec_no_match(&String::from_utf8_lossy(item.original()))
        );
        any = true;
    }
    any
}

/// Resolves the `--source` tree-ish: "could not resolve" when it names
/// nothing, "unable to read tree" when it is no tree-ish.
pub(crate) fn resolve_source(repo: &Repo, revs: &Revs, objects: &Objects, name: &str) -> Result<ObjectId, Die> {
    let id = match revs.resolve(name) {
        Some(id) => id,
        None => super::switch::resolve_commit(revs, objects, name)
            .ok_or_else(|| Die::Fatal(msg::could_not_resolve(name)))?,
    };
    tree_of(repo, objects, &id).ok_or_else(|| {
        let id = objects.peel(&id).unwrap_or(id);
        Die::Fatal(smsg::unable_to_read_tree(&id.to_hex()))
    })
}

#[allow(clippy::too_many_lines)]
pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let source = RefCell::new(None);
    let staged = Cell::new(false);
    let worktree = Cell::new(false);
    let ignore_unmerged = Cell::new(false);
    let overlay = Cell::new(false);
    let quiet = Cell::new(0);
    let recurse = RefCell::new(None::<String>);
    let progress = Cell::new(-1);
    let merge = Cell::new(false);
    let conflict = RefCell::new(None);
    let stage = Cell::new(0);
    let patch = Cell::new(false);
    let unified = Cell::new(-1);
    let inter_hunk = Cell::new(-1);
    let skip_worktree_bits = Cell::new(false);
    let from_file = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string('s', "source", &source, "tree-ish", msg::HELP_SOURCE),
            Opt::flag('S', "staged", &staged, msg::HELP_STAGED),
            Opt::flag('W', "worktree", &worktree, msg::HELP_WORKTREE),
            Opt::flag(None, "ignore-unmerged", &ignore_unmerged, msg::HELP_IGNORE_UNMERGED),
            Opt::flag(None, "overlay", &overlay, msg::HELP_OVERLAY),
            Opt::quiet(&quiet, smsg::HELP_QUIET),
            Opt::string(None, "recurse-submodules", &recurse, "checkout", smsg::HELP_RECURSE)
                .optional(Some("")),
            Opt::bool(None, "progress", &progress, smsg::HELP_PROGRESS),
            Opt::flag('m', "merge", &merge, smsg::HELP_MERGE),
            Opt::string(None, "conflict", &conflict, "style", smsg::HELP_CONFLICT),
            Opt::set_int('2', "ours", &stage, smsg::HELP_OURS, 2).noneg(),
            Opt::set_int('3', "theirs", &stage, smsg::HELP_THEIRS, 3).noneg(),
            Opt::flag('p', "patch", &patch, rmsg::HELP_PATCH),
            Opt::integer('U', "unified", &unified, rmsg::HELP_UNIFIED).noneg(),
            Opt::integer(None, "inter-hunk-context", &inter_hunk, rmsg::HELP_INTER_HUNK).noneg(),
            Opt::flag(
                None,
                "ignore-skip-worktree-bits",
                &skip_worktree_bits,
                smsg::HELP_SKIP_WORKTREE_BITS,
            ),
            Opt::string(None, "pathspec-from-file", &from_file, "file", rmsg::HELP_PATHSPEC_FROM_FILE),
            Opt::flag(None, "pathspec-file-nul", &file_nul, rmsg::HELP_PATHSPEC_FILE_NUL),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    check_conflict_style(conflict.borrow().as_deref())?;
    let from_file = from_file.borrow().clone();
    let merge = merge.get() || conflict.borrow().is_some();
    let stage = Stage::new(stage.get() as u8).filter(|s| s.is_conflict());

    if from_file.is_some() && !args.is_empty() {
        return Err(Die::Fatal(rmsg::FROM_FILE_WITH_ARGS.to_owned()));
    }
    if patch.get() {
        if from_file.is_some() {
            return Err(Die::Fatal(rmsg::FROM_FILE_WITH_PATCH.to_owned()));
        }
        if overlay.get() {
            return Err(Die::Fatal(msg::PATCH_WITH_OVERLAY.to_owned()));
        }
    }
    if file_nul.get() && from_file.is_none() {
        return Err(Die::Fatal(rmsg::FILE_NUL_WITHOUT_FROM_FILE.to_owned()));
    }
    if patch.get() {
        return Err(Die::Fatal(smsg::not_supported("git restore --patch")));
    }
    if recurse.borrow().is_some() {
        return Err(Die::Fatal(smsg::not_supported("git restore --recurse-submodules")));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;
    let staged = staged.get();
    // Without -S/-W, the work tree; -S alone restores only the index.
    let worktree = worktree.get() || !staged;
    let source_name = source.borrow().clone().or_else(|| staged.then(|| "HEAD".to_owned()));
    let tree = match &source_name {
        Some(name) => Some(resolve_source(&repo, &revs, &objects, name)?),
        None => None,
    };
    let specs: Vec<Vec<u8>> = match &from_file {
        Some(name) => read_pathspec_file(&repo, name, file_nul.get())?,
        None => args.into_iter().map(String::into_bytes).collect(),
    };
    if specs.is_empty() {
        return Err(Die::Fatal(msg::NO_PATHS.to_owned()));
    }
    if stage.is_some() && merge {
        return Err(Die::Fatal(msg::INDEX_MODES.to_owned()));
    }
    if staged && stage.is_some() {
        return Err(Die::Fatal(msg::OURS_WITH_STAGED.to_owned()));
    }
    if staged && merge {
        return Err(Die::Fatal(msg::MERGE_WITH_STAGED.to_owned()));
    }
    if tree.is_some() && (stage.is_some() || merge) {
        return Err(Die::Fatal(msg::FROM_TREE.to_owned()));
    }
    if merge {
        return Err(Die::Fatal(smsg::not_supported("git restore --merge")));
    }
    checkout_paths(
        repository::pathspec_globals(ctx),
        &repo,
        &objects,
        &specs,
        &PathCheckout {
            source: tree,
            worktree,
            index: staged,
            overlay: overlay.get(),
            stage,
            ignore_unmerged: ignore_unmerged.get(),
            ignore_skip_worktree: skip_worktree_bits.get(),
            report: false,
            quiet: quiet.get() > 0,
        },
    )
}
