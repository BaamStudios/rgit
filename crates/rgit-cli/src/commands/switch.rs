// SPDX-License-Identifier: MIT
//! `git switch` (Documentation/git-switch.adoc): `<branch>`, `--detach
//! [<commit>]`, `-c`/`-C <new-branch> [<start-point>]`, `--orphan`,
//! `--guess` (a new branch tracking the only matching remote-tracking
//! branch), `-t`/`--no-track`, `-f`/`--discard-changes` and `-q`. The work
//! is shared with `checkout` in `switch_branch`.
//!
//! Not yet: `-m`/`--merge`, `--conflict` (three-way merge, #158),
//! `--recurse-submodules`.

use rgit_hash::ObjectId;
use rgit_refs::RefStore;

use crate::messages::switch as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs};
use crate::repo::Mode;
use crate::repository::Repo;
use crate::switch_branch::{self as sb, Create, Request, Target};

/// `--track` / `--no-track`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Default,
    Yes,
    No,
}

/// What a branch-or-commit argument names.
pub enum Named {
    /// A local branch.
    Branch(String, ObjectId),
    /// A commit (any other revision).
    Commit(ObjectId),
    /// Nothing; with `--guess` the remote-tracking ref of the same name.
    Missing(Option<(String, ObjectId)>),
}

/// Expands `-` and `@{-<n>}` to the previously checked out name.
pub fn expand_previous(repo: &Repo, arg: &str) -> String {
    let n = if arg == "-" {
        Some(1)
    } else {
        arg.strip_prefix("@{-")
            .and_then(|r| r.strip_suffix('}'))
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|&n| n > 0)
    };
    match n.and_then(|n| sb::previous_checkout(repo, n)) {
        Some(name) => name,
        None => arg.to_owned(),
    }
}

/// Looks up `name` as a branch, then as a commit.
pub fn lookup(repo: &Repo, revs: &Revs, objects: &Objects, name: &str, guess: bool) -> Named {
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let full = format!("refs/heads/{name}");
    if let Ok(Some(id)) = store.resolve_oid(&full) {
        if rgit_refs::name::check_branch_name(name.as_bytes()).is_ok() {
            return Named::Branch(name.to_owned(), id);
        }
    }
    if let Some(id) = revs.resolve(name).and_then(|id| sb::peel_commit(objects, &id)) {
        return Named::Commit(id);
    }
    if guess {
        if let Some(r) = sb::guess_remote_branch(repo, &store, name) {
            if let Ok(Some(id)) = store.resolve_oid(&r) {
                return Named::Missing(Some((r, id)));
            }
        }
    }
    Named::Missing(None)
}

/// The upstream of a new branch from `start` under `track`.
pub fn upstream_for(repo: &Repo, start: &str, track: Track) -> Option<(String, String, String)> {
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    match track {
        Track::No => None,
        Track::Yes => sb::upstream_of_ref(repo, &store, start, true),
        Track::Default => sb::default_upstream(repo, &store, start),
    }
}

/// `-c`/`-b` and friends: the new branch `name` from `start` (or `HEAD`).
pub struct NewBranch {
    pub name: String,
    pub start: Option<String>,
    pub reset: bool,
}

/// Creates (or resets) a branch and switches to it. `invalid_start` is the
/// command's message for a start point that is not a commit.
pub fn create_and_switch(
    ctx: &Context,
    repo: &Repo,
    nb: NewBranch,
    track: Track,
    force: bool,
    quiet: bool,
    invalid_start: &dyn Fn(&str) -> String,
) -> CmdResult {
    sb::check_new_branch_name(repo, &nb.name)?;
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let full = format!("refs/heads/{}", nb.name);
    let exists = store.resolve_oid(&full).ok().flatten().is_some();
    if exists && !nb.reset {
        return Err(Die::Fatal(msg::branch_exists(&nb.name)));
    }
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;
    let (start_name, commit) = match &nb.start {
        Some(s) => {
            let s = expand_previous(repo, s);
            let id = revs
                .resolve(&s)
                .and_then(|id| sb::peel_commit(&objects, &id))
                .ok_or_else(|| Die::Fatal(invalid_start(&s)))?;
            (s, id)
        }
        None => match store.resolve_oid("HEAD").ok().flatten() {
            Some(id) => ("HEAD".to_owned(), id),
            None => {
                // An unborn branch: only HEAD moves.
                return sb::orphan(repo, &nb.name, false, quiet);
            }
        },
    };
    let track = match &nb.start {
        Some(_) => upstream_for(repo, &start_name, track),
        None => None,
    };
    sb::switch(
        repo,
        Request {
            target: Target::Branch(nb.name.clone()),
            commit,
            create: Some(Create {
                name: nb.name,
                start: start_name,
                reset: nb.reset,
                track,
                start_given: nb.start.is_some(),
            }),
            force,
            quiet,
            advise_detach: false,
        },
    )
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let create = RefCell::new(None);
    let force_create = RefCell::new(None);
    let guess = Cell::new(true);
    let discard = Cell::new(false);
    let quiet = Cell::new(0);
    let recurse = RefCell::new(None::<String>);
    let progress = Cell::new(-1);
    let merge = Cell::new(false);
    let conflict = RefCell::new(None);
    let detach = Cell::new(false);
    let track = Cell::new(Track::Default);
    let force = Cell::new(false);
    let orphan = RefCell::new(None);
    let overwrite_ignore = Cell::new(true);
    let ignore_other = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string('c', "create", &create, "branch", msg::HELP_CREATE),
            Opt::string('C', "force-create", &force_create, "branch", msg::HELP_FORCE_CREATE),
            Opt::flag(None, "guess", &guess, msg::HELP_GUESS_SWITCH),
            Opt::flag(None, "discard-changes", &discard, msg::HELP_DISCARD),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::string(None, "recurse-submodules", &recurse, "checkout", msg::HELP_RECURSE)
                .optional(Some("")),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::flag('m', "merge", &merge, msg::HELP_MERGE),
            Opt::string(None, "conflict", &conflict, "style", msg::HELP_CONFLICT),
            Opt::flag('d', "detach", &detach, msg::HELP_DETACH),
            Opt::callback(
                't',
                "track",
                Some("(direct|inherit)"),
                msg::HELP_TRACK,
                |_, unset| {
                    track.set(if unset { Track::No } else { Track::Yes });
                    Ok(())
                },
            )
            .optional(None)
            .literal_argh(),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::string(None, "orphan", &orphan, "new-branch", msg::HELP_ORPHAN),
            Opt::flag(None, "overwrite-ignore", &overwrite_ignore, msg::HELP_OVERWRITE_IGNORE),
            Opt::flag(None, "ignore-other-worktrees", &ignore_other, msg::HELP_IGNORE_OTHER),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let quiet = quiet.get() > 0;
    let force = force.get() || discard.get();
    let track = track.get();
    let create = create.borrow().clone();
    let force_create = force_create.borrow().clone();
    let orphan_name = orphan.borrow().clone();
    if merge.get() || conflict.borrow().is_some() {
        return Err(Die::Fatal(msg::not_supported("git switch --merge")));
    }
    if recurse.borrow().is_some() {
        return Err(Die::Fatal(msg::not_supported(
            "git switch --recurse-submodules",
        )));
    }
    if detach.get() && (create.is_some() || force_create.is_some() || orphan_name.is_some()) {
        return Err(Die::Fatal(msg::DETACH_WITH_CREATE.to_owned()));
    }
    if args.len() > 1 {
        return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    if let Some(name) = orphan_name {
        if !args.is_empty() {
            return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
        }
        return sb::orphan(&repo, &name, true, quiet);
    }
    if let Some(name) = force_create.or(create.clone()) {
        let reset = create.is_none();
        return create_and_switch(
            ctx,
            &repo,
            NewBranch {
                name,
                start: args.first().cloned(),
                reset,
            },
            track,
            force,
            quiet,
            &msg::invalid_ref,
        );
    }
    let Some(arg) = args.first().cloned().or_else(|| detach.get().then(|| "HEAD".to_owned()))
    else {
        return Err(Die::Fatal(msg::MISSING_BRANCH_OR_COMMIT.to_owned()));
    };
    let name = expand_previous(&repo, &arg);
    let objects = Objects::open(&repo.setup);
    let revs = Revs::new(ctx, &repo.setup)?;
    let named = if detach.get() {
        match revs.resolve(&name).and_then(|id| sb::peel_commit(&objects, &id)) {
            Some(id) => Named::Commit(id),
            None => Named::Missing(None),
        }
    } else {
        lookup(&repo, &revs, &objects, &name, guess.get())
    };
    match named {
        Named::Branch(b, id) => sb::switch(
            &repo,
            Request {
                target: Target::Branch(b),
                commit: id,
                create: None,
                force,
                quiet,
                advise_detach: false,
            },
        ),
        Named::Commit(id) if detach.get() => sb::switch(
            &repo,
            Request {
                target: Target::Detached { name },
                commit: id,
                create: None,
                force,
                quiet,
                advise_detach: false,
            },
        ),
        Named::Commit(_) => {
            let store = refs_support::ref_store(&repo.setup, &repo.config);
            let kind = if matches!(store.read_raw(&format!("refs/tags/{name}")), Ok(Some(_))) {
                "tag"
            } else if matches!(store.read_raw(&format!("refs/remotes/{name}")), Ok(Some(_))) {
                "remote branch"
            } else {
                "commit"
            };
            eprintln!("fatal: {}", msg::branch_expected(kind, &name));
            eprint!("{}", msg::hint_detach_branch_expected());
            Err(Die::Exit(128))
        }
        Named::Missing(Some((remote_ref, _))) => create_and_switch(
            ctx,
            &repo,
            NewBranch {
                name: name.clone(),
                start: Some(remote_ref.clone()),
                reset: false,
            },
            track,
            force,
            quiet,
            &msg::invalid_ref,
        ),
        Named::Missing(None) => Err(Die::Fatal(msg::invalid_ref(&name))),
    }
}
