// SPDX-License-Identifier: MIT
//! `git checkout` (Documentation/git-checkout.adoc).
//!
//! Implemented: `<branch>`, `<commit>`/`--detach [<commit>]` (with the
//! detached-HEAD advice), `-b`/`-B <new-branch> [<start-point>]`,
//! `--orphan`, `--guess` (`checkout.guess`, `checkout.defaultRemote`),
//! `-t`/`--track[=direct|inherit]`/`--no-track`, `-f`, `-q`, no argument (show
//! local changes and the upstream state), and paths: `[--] <pathspec>...`
//! from the index and `<tree-ish> [--] <pathspec>...` from a tree (overlay
//! mode). Branch switching is shared with `switch` (`switch_branch`).
//!
//! Not yet: `-m`/`--merge`, `--conflict`, `--ours`/`--theirs`, `-p`,
//! `--no-overlay`, `--pathspec-from-file`, `--recurse-submodules`.

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Stage};
use rgit_object::ObjectKind;
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_refs::RefStore;
use rgit_worktree::stat::{Status, smudge_racy_entries};

use super::switch::{self as sw, CreateOptions, Named, NewBranch};
use crate::commands::read_tree::{lock_failed, unpack_failed};
use crate::messages::switch as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs};
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::switch_branch::{self as sb, Request, Target};
use crate::tracking::{AutoSetup, Track};

/// A boolean option with only a long name.
fn long<'a>(name: &'a str, cell: &'a Cell<bool>, help: &'a str) -> Opt<'a> {
    Opt::flag(None, name, cell, help)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let new_b = RefCell::new(None);
    let new_bb = RefCell::new(None);
    let reflog = Cell::new(false);
    let guess = Cell::new(-1);
    let overlay = Cell::new(true);
    let auto_advance = Cell::new(false);
    let quiet = Cell::new(0);
    let recurse = RefCell::new(None::<String>);
    let progress = Cell::new(-1);
    let merge = Cell::new(false);
    let conflict = RefCell::new(None);
    let detach = Cell::new(false);
    let track = Cell::new(Track::Unspecified);
    let force = Cell::new(false);
    let orphan = RefCell::new(None);
    let overwrite_ignore = Cell::new(true);
    let ignore_other = Cell::new(false);
    let ours = Cell::new(false);
    let theirs = Cell::new(false);
    let patch = Cell::new(false);
    let unified = Cell::new(-1);
    let inter_hunk = Cell::new(-1);
    let skip_worktree_bits = Cell::new(false);
    let from_file = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::CHECKOUT_USAGE,
        vec![
            Opt::string('b', None, &new_b, "branch", msg::HELP_CHECKOUT_B),
            Opt::string('B', None, &new_bb, "branch", msg::HELP_CHECKOUT_FORCE_B),
            Opt::flag('l', None, &reflog, msg::HELP_REFLOG),
            Opt::bool(None, "guess", &guess, msg::HELP_GUESS_CHECKOUT),
            long("overlay", &overlay, msg::HELP_OVERLAY),
            long("auto-advance", &auto_advance, crate::messages::reset::HELP_AUTO_ADVANCE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::string(None, "recurse-submodules", &recurse, "checkout", msg::HELP_RECURSE)
                .optional(Some("")),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::flag('m', "merge", &merge, msg::HELP_MERGE),
            Opt::string(None, "conflict", &conflict, "style", msg::HELP_CONFLICT),
            Opt::flag('d', "detach", &detach, msg::HELP_DETACH),
            Track::option(&track),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::string(None, "orphan", &orphan, "new-branch", msg::HELP_ORPHAN),
            long("overwrite-ignore", &overwrite_ignore, msg::HELP_OVERWRITE_IGNORE),
            long("ignore-other-worktrees", &ignore_other, msg::HELP_IGNORE_OTHER),
            Opt::flag('2', "ours", &ours, msg::HELP_OURS).noneg(),
            Opt::flag('3', "theirs", &theirs, msg::HELP_THEIRS).noneg(),
            Opt::flag('p', "patch", &patch, crate::messages::reset::HELP_PATCH),
            Opt::integer('U', "unified", &unified, crate::messages::reset::HELP_UNIFIED).noneg(),
            Opt::integer(
                None,
                "inter-hunk-context",
                &inter_hunk,
                crate::messages::reset::HELP_INTER_HUNK,
            )
            .noneg(),
            long("ignore-skip-worktree-bits", &skip_worktree_bits, msg::HELP_SKIP_WORKTREE_BITS),
            Opt::string(
                None,
                "pathspec-from-file",
                &from_file,
                "file",
                crate::messages::reset::HELP_PATHSPEC_FROM_FILE,
            ),
            long("pathspec-file-nul", &file_nul, crate::messages::reset::HELP_PATHSPEC_FILE_NUL),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    drop(parser);
    let quiet = quiet.get() > 0;
    let force = force.get();
    let track = track.get();
    let mut new_b = new_b.borrow().clone();
    let new_bb = new_bb.borrow().clone();
    let orphan_name = orphan.borrow().clone();
    let unsupported = |what: &str| Err(Die::Fatal(msg::not_supported(what)));
    if merge.get() || conflict.borrow().is_some() {
        return unsupported("git checkout --merge");
    }
    if patch.get() {
        return unsupported("git checkout --patch");
    }
    if ours.get() || theirs.get() {
        return unsupported("git checkout --ours/--theirs");
    }
    if !overlay.get() {
        return unsupported("git checkout --no-overlay");
    }
    if from_file.borrow().is_some() {
        return unsupported("git checkout --pathspec-from-file");
    }
    if recurse.borrow().is_some() {
        return unsupported("git checkout --recurse-submodules");
    }
    if track.is_given() && orphan_name.is_some() {
        return Err(Die::Fatal(msg::ORPHAN_WITH_TRACK.to_owned()));
    }
    let implied = track.is_given() && new_b.is_none() && new_bb.is_none();
    let creating = implied || new_b.is_some() || new_bb.is_some() || orphan_name.is_some();
    if detach.get() && creating {
        return Err(Die::Fatal(msg::DETACH_WITH_CREATE.to_owned()));
    }

    let dashdash = args.iter().position(|a| a == "--");
    let (before, paths): (Vec<String>, Option<Vec<String>>) = match dashdash {
        Some(p) => (args[..p].to_vec(), Some(args[p + 1..].to_vec())),
        None => (args.clone(), None),
    };
    // `-t`/`--no-track` alone create a branch named after the start point.
    if implied {
        if before.len() > 1 || paths.as_ref().is_some_and(|p| !p.is_empty()) {
            return Err(Die::Fatal(msg::TRACK_WITH_PATHS.to_owned()));
        }
        new_b = Some(sw::implied_branch(&before, "b")?);
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let auto = AutoSetup::from_config(&repo.config)?;
    let guess = sw::guess_enabled(&repo, guess.get());
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;

    if let Some(name) = orphan_name {
        if before.len() > 1 || paths.as_ref().is_some_and(|p| !p.is_empty()) {
            return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
        }
        if let Some(start) = before.first() {
            return orphan_from(ctx, &repo, &name, start, quiet);
        }
        return sb::orphan(&repo, &name, false, quiet);
    }
    if let Some(name) = new_bb.clone().or(new_b.clone()) {
        if before.len() > 1 || paths.as_ref().is_some_and(|p| !p.is_empty()) {
            return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
        }
        let invalid = |s: &str| msg::not_a_commit_for_branch(s, &name);
        return sw::create_and_switch(
            &repo,
            &revs,
            NewBranch {
                name: name.clone(),
                start: before.first().cloned(),
                reset: new_b.is_none(),
            },
            &CreateOptions {
                track,
                auto,
                force,
                quiet,
                invalid_start: &invalid,
            },
        );
    }

    // `<tree-ish> -- <paths>` / `-- <paths>`.
    if let Some(paths) = paths {
        if before.len() > 1 {
            return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
        }
        if let Some(t) = before.first() {
            let name = sw::expand_previous(&repo, &revs, t)?;
            if paths.is_empty() {
                // `<branch> --`: a branch switch, with `--guess`.
                return switch_or_guess(&repo, &revs, &objects, &name, guess, track, auto, force, quiet, None);
            }
            let tree = revs
                .resolve(&name)
                .and_then(|id| tree_of(&repo, &objects, &id))
                .ok_or_else(|| Die::Fatal(msg::invalid_ref(&name)))?;
            return checkout_paths(repository::pathspec_globals(ctx), &repo, &objects, Some(tree), &paths, false, quiet);
        }
        if paths.is_empty() {
            return stay(&repo, force, quiet);
        }
        return checkout_paths(repository::pathspec_globals(ctx), &repo, &objects, None, &paths, false, quiet);
    }

    let Some(first) = before.first().cloned() else {
        if detach.get() {
            return switch_to(&repo, &revs, &objects, "HEAD", true, force, quiet, true);
        }
        return stay(&repo, force, quiet);
    };
    if detach.get() {
        if before.len() > 1 {
            return Err(Die::Fatal(msg::detach_takes_no_path(&before[1])));
        }
        let name = sw::expand_previous(&repo, &revs, &first)?;
        if revs
            .resolve(&name)
            .and_then(|id| sb::peel_commit(&objects, &id))
            .is_none()
        {
            return Err(Die::Fatal(msg::detach_takes_no_path(&first)));
        }
        return switch_to(&repo, &revs, &objects, &name, true, force, quiet, true);
    }
    let name = sw::expand_previous(&repo, &revs, &first)?;
    if before.len() > 1 {
        // `<tree-ish> <paths>...` when the first argument names a tree.
        if let Some(tree) = revs
            .resolve(&name)
            .and_then(|id| tree_of(&repo, &objects, &id))
        {
            return checkout_paths(repository::pathspec_globals(ctx), &repo, &objects, Some(tree), &before[1..], true, quiet);
        }
        return checkout_paths(repository::pathspec_globals(ctx), &repo, &objects, None, &before, true, quiet);
    }
    if name == "HEAD" {
        return stay(&repo, force, quiet);
    }
    let globals = repository::pathspec_globals(ctx);
    switch_or_guess(&repo, &revs, &objects, &name, guess, track, auto, force, quiet, Some((globals, &before)))
}

/// `checkout <name>` (`paths`: without `--`, falling back to restoring
/// the paths `paths.1` when `name` names nothing): the branch or commit,
/// else a new branch from the remote-tracking branch `--guess` finds.
#[allow(clippy::too_many_arguments)]
fn switch_or_guess(
    repo: &Repo,
    revs: &Revs,
    objects: &Objects,
    name: &str,
    guess: bool,
    track: Track,
    auto: AutoSetup,
    force: bool,
    quiet: bool,
    paths: Option<(rgit_pathspec::GlobalFlags, &[String])>,
) -> CmdResult {
    match sw::lookup(repo, revs, objects, name, guess, "checkout")? {
        Named::Branch(..) | Named::Commit(_) => {
            switch_to(repo, revs, objects, name, false, force, quiet, false)
        }
        Named::Missing(Some(remote_ref)) => sw::create_and_switch(
            repo,
            revs,
            NewBranch {
                name: name.to_owned(),
                start: Some(remote_ref),
                reset: false,
            },
            &CreateOptions {
                track,
                auto,
                force,
                quiet,
                invalid_start: &|s: &str| msg::not_a_commit_for_branch(s, name),
            },
        ),
        Named::Missing(None) => {
            if let Some(id) = revs.resolve(name) {
                let id = objects.peel(&id).unwrap_or(id);
                return Err(Die::Fatal(msg::unable_to_read_tree(&id.to_hex())));
            }
            match paths {
                Some((globals, specs)) => {
                    checkout_paths(globals, repo, objects, None, specs, true, quiet)
                }
                None => Err(Die::Fatal(msg::invalid_ref(name))),
            }
        }
    }
}

/// The tree of the tree-ish `id`.
fn tree_of(repo: &Repo, objects: &Objects, id: &ObjectId) -> Option<ObjectId> {
    let p = objects.peel(id)?;
    match objects.kind(&p)? {
        ObjectKind::Tree => Some(p),
        ObjectKind::Commit => sb::commit_tree(repo, &p),
        _ => None,
    }
}

/// Switches to the branch or commit `name` (`detach`: always detached).
#[allow(clippy::too_many_arguments)]
fn switch_to(
    repo: &Repo,
    revs: &Revs,
    objects: &Objects,
    name: &str,
    detach: bool,
    force: bool,
    quiet: bool,
    explicit_detach: bool,
) -> CmdResult {
    let named = if detach {
        match revs.resolve(name).and_then(|id| sb::peel_commit(objects, &id)) {
            Some(id) => Named::Commit(id),
            None => Named::Missing(None),
        }
    } else {
        sw::lookup(repo, revs, objects, name, false, "checkout")?
    };
    let (target, commit) = match named {
        Named::Branch(b, id) => (Target::Branch(b), id),
        Named::Commit(id) => (
            Target::Detached {
                name: name.to_owned(),
            },
            id,
        ),
        Named::Missing(_) => return Err(Die::Fatal(msg::invalid_ref(name))),
    };
    sb::switch(
        repo,
        revs,
        Request {
            target,
            commit,
            create: None,
            force,
            quiet,
            advise_detach: !explicit_detach,
        },
    )
}

/// `checkout` without a branch: re-applies `HEAD` (with `-f` discarding
/// local changes), lists local changes and the upstream state.
fn stay(repo: &Repo, force: bool, quiet: bool) -> CmdResult {
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let Some(head) = store.resolve_oid("HEAD").ok().flatten() else {
        return Ok(0);
    };
    let tree = sb::commit_tree(repo, &head)
        .ok_or_else(|| Die::Fatal(msg::unable_to_read_tree(&head.to_hex())))?;
    let moved = sb::move_index(repo, tree, tree, force)?;
    if !quiet {
        use std::io::Write;
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&moved.changes);
        let _ = o.flush();
        if let Ok(Some(rgit_refs::Target::Symbolic(r))) = store.read_raw("HEAD") {
            if let Some(b) = r.strip_prefix("refs/heads/") {
                drop(o);
                sb::print_tracking(repo, b, head);
            }
        }
    }
    Ok(0)
}

/// `checkout --orphan <name> <start>`: index and work tree move to
/// `start`, then `HEAD` becomes the unborn branch.
fn orphan_from(ctx: &Context, repo: &Repo, name: &str, start: &str, quiet: bool) -> CmdResult {
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;
    let id = revs
        .resolve(start)
        .and_then(|id| sb::peel_commit(&objects, &id))
        .ok_or_else(|| Die::Fatal(msg::not_a_commit_for_branch(start, name)))?;
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let empty = ObjectId::empty_tree(repo.hash);
    let old = store
        .resolve_oid("HEAD")
        .ok()
        .flatten()
        .and_then(|o| sb::commit_tree(repo, &o))
        .unwrap_or(empty);
    let new = sb::commit_tree(repo, &id).unwrap_or(empty);
    sb::move_index(repo, old, new, false)?;
    sb::orphan(repo, name, false, quiet)
}

/// Restores the files matching `specs` from the index (`tree` is `None`)
/// or from `tree` (which also updates their index entries). `report`:
/// print "Updated N paths ..." (git does when no `--` was given).
fn checkout_paths(
    globals: rgit_pathspec::GlobalFlags,
    repo: &Repo,
    objects: &Objects,
    tree: Option<ObjectId>,
    specs: &[String],
    report: bool,
    quiet: bool,
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

    // The entries to write, by path.
    let mut targets: Vec<Vec<u8>> = Vec::new();
    let mut count = 0usize;
    match tree {
        Some(t) => {
            let entries = rgit_unpack::tree_entries(repo.odb(), &t).map_err(unpack_failed)?;
            let chosen: Vec<Entry> = entries
                .into_iter()
                .filter(|e| ps.match_path_seen(e.path(), &mut seen).is_some())
                .collect();
            if report_unmatched(&ps, &seen) {
                return Err(Die::Exit(1));
            }
            for e in chosen {
                let cur = index.entries_for_path(e.path());
                let same = cur.len() == 1
                    && cur[0].stage() == Stage::MERGED
                    && cur[0].mode == e.mode
                    && cur[0].id == e.id
                    && !cur[0].flags.intent_to_add;
                let clean = same
                    && matches!(checker.check(&cur[0]), Ok(Status::Unchanged));
                if clean {
                    continue;
                }
                let mut ne = e.clone();
                if same {
                    ne.stat = cur[0].stat;
                }
                index.remove_path(e.path());
                index
                    .add(
                        ne,
                        AddOptions {
                            ok_to_replace: true,
                            ..Default::default()
                        },
                    )
                    .map_err(|err| Die::Fatal(err.to_string()))?;
                targets.push(e.path().to_vec());
                count += 1;
            }
            index.set_cache_tree(None);
            let ct = rgit_unpack::repair_cache_tree(&index, repo.odb());
            index.set_cache_tree(Some(ct));
        }
        None => {
            let mut unmerged: Vec<Vec<u8>> = Vec::new();
            for e in index.entries() {
                if ps.match_path_seen(e.path(), &mut seen).is_none() {
                    continue;
                }
                if e.stage().is_conflict() {
                    if unmerged.last().map(Vec::as_slice) != Some(e.path()) {
                        unmerged.push(e.path().to_vec());
                    }
                    continue;
                }
                if e.flags.intent_to_add || e.flags.skip_worktree {
                    continue;
                }
                if !matches!(checker.check(e), Ok(Status::Unchanged)) {
                    targets.push(e.path().to_vec());
                }
            }
            if report_unmatched(&ps, &seen) {
                return Err(Die::Exit(1));
            }
            if !unmerged.is_empty() {
                for p in &unmerged {
                    eprintln!("error: {}", msg::path_unmerged(&String::from_utf8_lossy(p)));
                }
                return Err(Die::Exit(1));
            }
            count = targets.len();
        }
    }

    let symlinks = checker.options().symlinks;
    let mut failed = false;
    for path in &targets {
        let Ok(pos) = index.position(path, Stage::MERGED) else {
            continue;
        };
        let entry = index.entries()[pos].clone();
        let data = if entry.mode.is_gitlink() {
            Vec::new()
        } else {
            match rgit_unpack::read_blob(repo.odb(), &entry.id) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: {e}");
                    failed = true;
                    continue;
                }
            }
        };
        match rgit_unpack::write_entry(&work_tree, path, &entry, &data, symlinks) {
            Ok(Some(meta)) => {
                if let Some(e) = index.entry_mut(pos) {
                    e.stat = rgit_worktree::stat::stat_from_metadata(&meta);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("error: {e}");
                failed = true;
            }
        }
    }
    if let Some(reference) = mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    sb::write_index(repo, &index, lock)?;
    if report && !quiet {
        match tree {
            Some(t) => eprintln!("{}", msg::updated_paths_from(count, &objects.abbrev(&t, 7))),
            None => eprintln!("{}", msg::updated_paths_from_index(count)),
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
            msg::pathspec_no_match(&String::from_utf8_lossy(item.original()))
        );
        any = true;
    }
    any
}
