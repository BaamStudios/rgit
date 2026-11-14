// SPDX-License-Identifier: MIT
//! `git checkout` (Documentation/git-checkout.adoc).
//!
//! Implemented: `<branch>`, `<commit>`/`--detach [<commit>]` (with the
//! detached-HEAD advice), `-b`/`-B <new-branch> [<start-point>]`,
//! `--orphan`, `--guess` (`checkout.guess`, `checkout.defaultRemote`),
//! `-t`/`--track[=direct|inherit]`/`--no-track`, `-f`, `-q`, no argument (show
//! local changes and the upstream state), and paths: `[--] <pathspec>...`
//! from the index and `<tree-ish> [--] <pathspec>...` from a tree, with
//! `--overlay`/`--no-overlay`, `--ours`/`--theirs` and
//! `--pathspec-from-file`. Branch switching is shared with `switch`
//! (`switch_branch`), checking out paths with `restore`.
//!
//! Not yet: `-m`/`--merge`, `--conflict`, `-p`, `--recurse-submodules`.

use rgit_hash::ObjectId;
use rgit_index::Stage;
use rgit_pathspec::GlobalFlags;
use rgit_refs::RefStore;

use super::restore::{self as rs, PathCheckout};
use super::switch::{self as sw, CreateOptions, Named, NewBranch};
use crate::messages::reset as rmsg;
use crate::messages::restore as rsmsg;
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

/// The options of the path mode, checked once `checkout` knows whether it
/// switches branches or checks out paths.
struct Modes {
    /// `--ours`/`--theirs`.
    stage: Option<Stage>,
    /// `--overlay`/`--no-overlay` when given.
    overlay: Option<bool>,
    /// `-m`/`--conflict`.
    merge: bool,
    force: bool,
    quiet: bool,
    ignore_skip_worktree: bool,
    globals: GlobalFlags,
}

impl Modes {
    /// Before switching branches; `bare`: no argument at all.
    fn switching(&self, bare: bool) -> Result<(), Die> {
        if self.stage.is_some() {
            let m = if bare {
                rsmsg::OURS_NEEDS_PATHS
            } else {
                rsmsg::OURS_WITH_SWITCHING
            };
            return Err(Die::Fatal(m.to_owned()));
        }
        if self.overlay.is_some() {
            return Err(Die::Fatal(rsmsg::OVERLAY_WITH_SWITCHING.to_owned()));
        }
        if self.merge {
            return Err(Die::Fatal(msg::not_supported("git checkout --merge")));
        }
        Ok(())
    }

    /// Checks out the paths `specs` from `tree` or the index. `report`:
    /// print "Updated N paths ..." (git does when no `--` was given).
    fn paths<S: AsRef<[u8]>>(
        &self,
        repo: &Repo,
        objects: &Objects,
        tree: Option<ObjectId>,
        specs: &[S],
        report: bool,
    ) -> CmdResult {
        let given = [self.force, self.stage.is_some(), self.merge];
        if given.iter().filter(|&&g| g).count() > 1 {
            return Err(Die::Fatal(rsmsg::INDEX_MODES.to_owned()));
        }
        if tree.is_some() && (self.stage.is_some() || self.merge) {
            return Err(Die::Fatal(rsmsg::FROM_TREE.to_owned()));
        }
        if self.merge {
            return Err(Die::Fatal(msg::not_supported("git checkout --merge")));
        }
        let rc = rs::checkout_paths(
            self.globals,
            repo,
            objects,
            specs,
            &PathCheckout {
                source: tree,
                worktree: true,
                index: tree.is_some(),
                overlay: self.overlay.unwrap_or(true),
                stage: self.stage,
                ignore_unmerged: self.force,
                ignore_skip_worktree: self.ignore_skip_worktree,
                report: report && !self.quiet,
                quiet: self.quiet,
            },
        )?;
        // `post-checkout` with `HEAD` twice and flag 0 (not `restore`).
        let head = refs_support::ref_store(&repo.setup, &repo.config)
            .resolve_oid("HEAD")
            .ok()
            .flatten();
        let code = crate::hooks::post_checkout(repo, head, head, false)?;
        Ok(if rc != 0 { rc } else { code })
    }
}

#[allow(clippy::too_many_lines)]
pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let new_b = RefCell::new(None);
    let new_bb = RefCell::new(None);
    let reflog = Cell::new(false);
    let guess = Cell::new(-1);
    let overlay = Cell::new(-1);
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
    let stage = Cell::new(0);
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
            Opt::bool(None, "overlay", &overlay, msg::HELP_OVERLAY),
            long("auto-advance", &auto_advance, rmsg::HELP_AUTO_ADVANCE),
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
            Opt::set_int('2', "ours", &stage, msg::HELP_OURS, 2).noneg(),
            Opt::set_int('3', "theirs", &stage, msg::HELP_THEIRS, 3).noneg(),
            Opt::flag('p', "patch", &patch, rmsg::HELP_PATCH),
            Opt::integer('U', "unified", &unified, rmsg::HELP_UNIFIED).noneg(),
            Opt::integer(None, "inter-hunk-context", &inter_hunk, rmsg::HELP_INTER_HUNK).noneg(),
            long("ignore-skip-worktree-bits", &skip_worktree_bits, msg::HELP_SKIP_WORKTREE_BITS),
            Opt::string(None, "pathspec-from-file", &from_file, "file", rmsg::HELP_PATHSPEC_FROM_FILE),
            long("pathspec-file-nul", &file_nul, rmsg::HELP_PATHSPEC_FILE_NUL),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    drop(parser);
    rs::check_conflict_style(conflict.borrow().as_deref())?;
    let quiet = quiet.get() > 0;
    let force = force.get();
    let track = track.get();
    let mut new_b = new_b.borrow().clone();
    let new_bb = new_bb.borrow().clone();
    let orphan_name = orphan.borrow().clone();
    let from_file = from_file.borrow().clone();
    let creating = [new_b.is_some(), new_bb.is_some(), orphan_name.is_some()];
    if creating.iter().filter(|&&c| c).count() > 1 {
        return Err(Die::Fatal(msg::CREATE_OPTIONS_CHECKOUT.to_owned()));
    }
    let unsupported = |what: &str| Err(Die::Fatal(msg::not_supported(what)));
    if file_nul.get() && from_file.is_none() {
        return Err(Die::Fatal(rmsg::FILE_NUL_WITHOUT_FROM_FILE.to_owned()));
    }
    if patch.get() {
        if from_file.is_some() {
            return Err(Die::Fatal(rmsg::FROM_FILE_WITH_PATCH.to_owned()));
        }
        if overlay.get() == 1 {
            return Err(Die::Fatal(rsmsg::PATCH_WITH_OVERLAY.to_owned()));
        }
        return unsupported("git checkout --patch");
    }
    if recurse.borrow().is_some() {
        return unsupported("git checkout --recurse-submodules");
    }
    if from_file.is_some() && detach.get() {
        return Err(Die::Fatal(rsmsg::FROM_FILE_WITH_DETACH.to_owned()));
    }
    let modes = Modes {
        stage: Stage::new(stage.get() as u8).filter(|s| s.is_conflict()),
        overlay: (overlay.get() >= 0).then_some(overlay.get() == 1),
        merge: merge.get() || conflict.borrow().is_some(),
        force,
        quiet,
        ignore_skip_worktree: skip_worktree_bits.get(),
        globals: repository::pathspec_globals(ctx),
    };
    let implied = track.is_given() && new_b.is_none() && new_bb.is_none() && orphan_name.is_none();
    let creating = implied || new_b.is_some() || new_bb.is_some() || orphan_name.is_some();
    if detach.get() && creating {
        return Err(Die::Fatal(msg::DETACH_WITH_CREATE.to_owned()));
    }

    let dashdash = args.iter().position(|a| a == "--");
    let (before, paths): (Vec<String>, Option<Vec<String>>) = match dashdash {
        Some(p) => (args[..p].to_vec(), Some(args[p + 1..].to_vec())),
        None => (args.clone(), None),
    };
    let paths_given = paths.as_ref().is_some_and(|p| !p.is_empty());
    // A start point followed by paths.
    let start_and_paths = before.len() > 1 || (before.len() == 1 && paths_given);
    // `-t`/`--no-track` alone create a branch named after the start point.
    if implied {
        new_b = Some(sw::implied_branch(&before, "b")?);
    }
    if creating && (start_and_paths || (implied && paths_given)) {
        if track.is_given() {
            return Err(Die::Fatal(msg::TRACK_WITH_PATHS.to_owned()));
        }
        if reflog.get() {
            return Err(Die::Fatal(msg::REFLOG_WITH_PATHS.to_owned()));
        }
    }
    if track.is_given() && orphan_name.is_some() {
        return Err(Die::Fatal(msg::ORPHAN_WITH_TRACK.to_owned()));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let auto = AutoSetup::from_config(&repo.config)?;
    let guess = sw::guess_enabled(&repo, guess.get());
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;

    // `--pathspec-from-file`: the paths come from the file, the arguments
    // name at most the tree-ish. An empty file means no paths.
    let file_specs = match &from_file {
        Some(name) => {
            if paths_given || before.len() > 1 {
                return Err(Die::Fatal(rmsg::FROM_FILE_WITH_ARGS.to_owned()));
            }
            let specs = rs::read_pathspec_file(&repo, name, file_nul.get())?;
            (!specs.is_empty()).then_some(specs)
        }
        None => None,
    };

    // `-b`/`-B`/`--orphan` with a start point and paths.
    let with_paths = |name: &str| -> Result<(), Die> {
        if start_and_paths || file_specs.is_some() {
            return Err(Die::Fatal(msg::cannot_update_paths_and_switch(name)));
        }
        if paths_given {
            return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
        }
        modes.switching(false)
    };
    if let Some(name) = orphan_name {
        let name = sw::expand_new_name(&repo, &name);
        with_paths(&name)?;
        if let Some(start) = before.first() {
            return orphan_from(ctx, &repo, &name, start, quiet, reflog.get());
        }
        return sb::orphan(&repo, &name, false, quiet, reflog.get());
    }
    if let Some(name) = new_bb.clone().or(new_b.clone()) {
        with_paths(&sw::expand_new_name(&repo, &name))?;
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
                reflog: reflog.get(),
                refuse_in_progress: false,
                invalid_start: &invalid,
            },
        );
    }

    if let Some(specs) = file_specs {
        let tree = match before.first() {
            Some(t) => {
                let name = sw::expand_previous(&repo, &revs, t)?;
                let tree = revs
                    .resolve(&name)
                    .and_then(|id| rs::tree_of(&repo, &objects, &id))
                    .ok_or_else(|| Die::Fatal(rmsg::FROM_FILE_WITH_ARGS.to_owned()))?;
                Some(tree)
            }
            None => None,
        };
        let report = dashdash.is_none() && !before.is_empty();
        return modes.paths(&repo, &objects, tree, &specs, report);
    }

    // `<tree-ish> -- <paths>` / `-- <paths>`.
    if let Some(paths) = paths {
        if before.len() > 1 {
            return Err(Die::Fatal(msg::one_reference_expected_given(before.len())));
        }
        if let Some(t) = before.first() {
            let name = sw::expand_previous(&repo, &revs, t)?;
            if paths.is_empty() && is_head(&name) {
                modes.switching(false)?;
                return stay(&repo, force, quiet);
            }
            if paths.is_empty() {
                // `<branch> --`: a branch switch, with `--guess`.
                modes.switching(false)?;
                return switch_or_guess(&repo, &revs, &objects, &name, guess, track, auto, &modes, false);
            }
            let tree = revs
                .resolve(&name)
                .and_then(|id| rs::tree_of(&repo, &objects, &id))
                .ok_or_else(|| Die::Fatal(msg::invalid_ref(&name)))?;
            return modes.paths(&repo, &objects, Some(tree), &paths, false);
        }
        if paths.is_empty() {
            modes.switching(true)?;
            return stay(&repo, force, quiet);
        }
        return modes.paths(&repo, &objects, None, &paths, false);
    }

    let Some(first) = before.first().cloned() else {
        if detach.get() {
            modes.switching(false)?;
            return switch_to(&repo, &revs, &objects, "HEAD", true, force, quiet, true);
        }
        modes.switching(true)?;
        return stay(&repo, force, quiet);
    };
    if detach.get() {
        if before.len() > 1 {
            return Err(Die::Fatal(msg::detach_takes_no_path(&before[1])));
        }
        let name = sw::expand_previous(&repo, &revs, &first)?;
        if sw::resolve_commit(&revs, &objects, &name).is_none() {
            return Err(Die::Fatal(msg::detach_takes_no_path(&first)));
        }
        modes.switching(false)?;
        return switch_to(&repo, &revs, &objects, &name, true, force, quiet, true);
    }
    let name = sw::expand_previous(&repo, &revs, &first)?;
    if before.len() > 1 {
        // `<tree-ish> <paths>...` when the first argument names a tree.
        if let Some(tree) = revs
            .resolve(&name)
            .and_then(|id| rs::tree_of(&repo, &objects, &id))
        {
            // Without `--` a revision must not also name a file.
            if !first.starts_with('-') && names_file(&repo, &first) {
                return Err(Die::Fatal(rmsg::both_revision_and_filename(&first)));
            }
            return modes.paths(&repo, &objects, Some(tree), &before[1..], true);
        }
        return modes.paths(&repo, &objects, None, &before, true);
    }
    if is_head(&name) {
        modes.switching(false)?;
        return stay(&repo, force, quiet);
    }
    switch_or_guess(&repo, &revs, &objects, &name, guess, track, auto, &modes, true)
}

/// `checkout <name>` (`paths`: without `--`, falling back to restoring
/// the path `name` when it names nothing): the branch or commit, else a
/// new branch from the remote-tracking branch `--guess` finds.
#[allow(clippy::too_many_arguments)]
fn switch_or_guess(
    repo: &Repo,
    revs: &Revs,
    objects: &Objects,
    name: &str,
    guess: bool,
    track: Track,
    auto: AutoSetup,
    modes: &Modes,
    paths: bool,
) -> CmdResult {
    let (force, quiet) = (modes.force, modes.quiet);
    match sw::lookup(repo, revs, objects, name, guess, "checkout")? {
        named @ (Named::Branch(..) | Named::Commit(_)) => {
            modes.switching(false)?;
            switch_named(repo, revs, name, named, force, quiet, false)
        }
        Named::Missing(Some(_)) if paths && names_file(repo, name) => {
            Err(Die::Fatal(msg::file_and_tracking_branch(name)))
        }
        Named::Missing(Some(remote_ref)) => {
            modes.switching(false)?;
            sw::create_and_switch(
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
                    reflog: false,
                    refuse_in_progress: false,
                    invalid_start: &|s: &str| msg::not_a_commit_for_branch(s, name),
                },
            )
        }
        Named::Missing(None) => {
            if let Some(id) = revs.resolve(name) {
                let id = objects.peel(&id).unwrap_or(id);
                return Err(Die::Fatal(msg::unable_to_read_tree(&id.to_hex())));
            }
            if paths {
                modes.paths(repo, objects, None, &[name], true)
            } else {
                Err(Die::Fatal(msg::invalid_ref(name)))
            }
        }
    }
}

/// `HEAD` or its short form `@`: `checkout` stays where it is.
fn is_head(name: &str) -> bool {
    name == "HEAD" || name == "@"
}

/// Whether the command-line argument `arg` names an existing file (git's
/// check before it takes an argument as a revision without `--`).
fn names_file(repo: &Repo, arg: &str) -> bool {
    if arg.starts_with(':') {
        return false;
    }
    let path = match &repo.setup.prefix {
        Some(p) if !std::path::Path::new(arg).is_absolute() => std::path::Path::new(p).join(arg),
        _ => std::path::PathBuf::from(arg),
    };
    std::fs::symlink_metadata(path).is_ok()
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
        match sw::resolve_commit(revs, objects, name) {
            Some(id) => Named::Commit(id),
            None => Named::Missing(None),
        }
    } else {
        sw::lookup(repo, revs, objects, name, false, "checkout")?
    };
    switch_named(repo, revs, name, named, force, quiet, explicit_detach)
}

/// Switches to what [`sw::lookup`] found for `name`.
fn switch_named(
    repo: &Repo,
    revs: &Revs,
    name: &str,
    named: Named,
    force: bool,
    quiet: bool,
    explicit_detach: bool,
) -> CmdResult {
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
            refuse_in_progress: false,
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
    let cancelled = crate::commands::reset::remove_branch_state(&repo.setup.git_dir, repo.hash);
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
    sb::warn_cancelled(cancelled, quiet);
    let code = crate::hooks::post_checkout(repo, Some(head), Some(head), true)?;
    Ok(i32::from(code != 0))
}

/// `checkout --orphan <name> <start>`: index and work tree move to
/// `start`, then `HEAD` becomes the unborn branch.
fn orphan_from(
    ctx: &Context,
    repo: &Repo,
    name: &str,
    start: &str,
    quiet: bool,
    reflog: bool,
) -> CmdResult {
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
    sb::orphan(repo, name, false, quiet, reflog)
}

