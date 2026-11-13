// SPDX-License-Identifier: MIT
//! `git switch` (Documentation/git-switch.adoc): `<branch>`, `--detach
//! [<commit>]`, `-c`/`-C <new-branch> [<start-point>]`, `--orphan`,
//! `--guess` (a new branch tracking the only matching remote-tracking
//! branch, `checkout.guess`, `checkout.defaultRemote`),
//! `-t`/`--track[=direct|inherit]`/`--no-track` (with the upstream setup
//! shared with `branch` in `tracking`), `-f`/`--discard-changes` and `-q`.
//! The work is shared with `checkout` in `switch_branch`.
//!
//! Not yet: `-m`/`--merge`, `--conflict` (three-way merge, #158),
//! `--recurse-submodules`.

use rgit_hash::ObjectId;
use rgit_refs::RefStore;

use crate::messages::switch as msg;
use crate::messages::tracking as tmsg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs};
use crate::repo::Mode;
use crate::repository::Repo;
use crate::switch_branch::{self as sb, Create, Request, Target};
use crate::tracking::{self, AutoSetup, Guess, Track};

/// What a branch-or-commit argument names.
pub enum Named {
    /// A local branch.
    Branch(String, ObjectId),
    /// A commit (any other revision).
    Commit(ObjectId),
    /// Nothing; with `--guess` the remote-tracking ref (full name) to
    /// create a branch of the same name from.
    Missing(Option<String>),
}

/// Expands `-` and `@{-<n>}` to the previously checked out name, and
/// `[<branch>]@{upstream}`/`@{push}` naming a local branch to that
/// branch (other upstreams stay as given and resolve to their commit).
pub fn expand_previous(repo: &Repo, revs: &Revs, arg: &str) -> Result<String, Die> {
    if let Some(full) = tracking_ref(revs, arg)? {
        if let Some(short) = full.strip_prefix("refs/heads/") {
            return Ok(short.to_owned());
        }
    }
    Ok(previous_name(repo, arg))
}

/// The full ref `[<branch>]@{upstream}`/`@{push}` stands for; `None`
/// for other names. A missing upstream is fatal, as in git.
pub fn tracking_ref(revs: &Revs, arg: &str) -> Result<Option<String>, Die> {
    if !arg.contains("@{") || arg.contains("@{-") {
        return Ok(None);
    }
    let d = revs
        .with_resolver(|r| r.dwim_ref(arg))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok((d.count == 1).then_some(d.full_name))
}

/// `-` and `@{-<n>}`.
fn previous_name(repo: &Repo, arg: &str) -> String {
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

/// `--[no-]guess` (`-1` when not given) over `checkout.guess` (default
/// true).
pub fn guess_enabled(repo: &Repo, option: i32) -> bool {
    if option >= 0 {
        return option > 0;
    }
    repo.config
        .bool("checkout.guess")
        .ok()
        .flatten()
        .unwrap_or(true)
}

/// Looks up `name` as a branch, then as a commit, then (with `guess`) as
/// the branch of exactly one remote. Several remotes having it is an error
/// (`command` names `switch` or `checkout` in the hint).
pub fn lookup(
    repo: &Repo,
    revs: &Revs,
    objects: &Objects,
    name: &str,
    guess: bool,
    command: &str,
) -> Result<Named, Die> {
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let full = format!("refs/heads/{name}");
    if let Ok(Some(id)) = store.resolve_oid(&full) {
        if rgit_refs::name::check_branch_name(name.as_bytes()).is_ok() {
            return Ok(Named::Branch(name.to_owned(), id));
        }
    }
    if let Some(id) = revs.resolve(name).and_then(|id| sb::peel_commit(objects, &id)) {
        return Ok(Named::Commit(id));
    }
    if guess && rgit_refs::name::check_branch_name(name.as_bytes()).is_ok() {
        let trepo = tracking::Repo {
            setup: &repo.setup,
            config: &repo.config,
            store: &store,
            revs,
        };
        match tracking::guess_remote_branch(&trepo, name)? {
            Guess::Unique(r) => return Ok(Named::Missing(Some(r))),
            Guess::Ambiguous(remotes) => {
                return Err(tracking::ambiguous_guess(
                    &repo.config,
                    name,
                    &remotes,
                    command,
                ));
            }
            Guess::None => {}
        }
    }
    Ok(Named::Missing(None))
}

/// `-t`/`--no-track` without `-c`/`-b`: the branch to create, named
/// after the start point `args[0]` (`create_option` is `c` or `b` for
/// the "try -c" message).
pub fn implied_branch(args: &[String], create_option: &str) -> Result<String, Die> {
    let Some(start) = args.first() else {
        return Err(Die::Fatal(tmsg::TRACK_NEEDS_NAME.to_owned()));
    };
    tracking::derive_branch_name(start).ok_or_else(|| tracking::missing_branch_name(create_option))
}

/// `-c`/`-b` and friends: the new branch `name` from `start` (or `HEAD`).
pub struct NewBranch {
    pub name: String,
    pub start: Option<String>,
    pub reset: bool,
}

/// How the new branch is set up.
pub struct CreateOptions<'a> {
    pub track: Track,
    pub auto: AutoSetup,
    pub force: bool,
    pub quiet: bool,
    /// The command's message for a start point that is not a commit.
    pub invalid_start: &'a dyn Fn(&str) -> String,
}

/// Creates (or resets) a branch and switches to it.
pub fn create_and_switch(
    repo: &Repo,
    revs: &Revs,
    nb: NewBranch,
    opts: &CreateOptions<'_>,
) -> CmdResult {
    sb::check_new_branch_name(repo, &nb.name)?;
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let full = format!("refs/heads/{}", nb.name);
    let exists = store.resolve_oid(&full).ok().flatten().is_some();
    if exists && !nb.reset {
        return Err(Die::Fatal(msg::branch_exists(&nb.name)));
    }
    let objects = Objects::open(&repo.setup);
    let (start_name, commit) = match &nb.start {
        Some(s) => {
            let s = expand_previous(repo, revs, s)?;
            let id = revs
                .resolve(&s)
                .and_then(|id| sb::peel_commit(&objects, &id))
                .ok_or_else(|| Die::Fatal((opts.invalid_start)(&s)))?;
            (s, id)
        }
        None => match store.resolve_oid("HEAD").ok().flatten() {
            Some(id) => ("HEAD".to_owned(), id),
            None => {
                // An unborn branch: only HEAD moves.
                return sb::orphan(repo, &nb.name, false, opts.quiet);
            }
        },
    };
    // Without a start point the new branch tracks the current one (when
    // `--track` or `branch.autoSetupMerge=always` says so).
    let track_start = match &nb.start {
        Some(_) => start_name.clone(),
        None => tracking::default_start(&store),
    };
    let trepo = tracking::Repo {
        setup: &repo.setup,
        config: &repo.config,
        store: &store,
        revs,
    };
    let plan = tracking::plan(&trepo, &nb.name, &track_start, true, opts.track, &opts.auto)?;
    sb::switch(
        repo,
        revs,
        Request {
            target: Target::Branch(nb.name.clone()),
            commit,
            create: Some(Create {
                name: nb.name,
                start: start_name,
                reset: nb.reset,
                track: plan.map(|p| (p, opts.auto)),
                start_given: nb.start.is_some(),
            }),
            force: opts.force,
            quiet: opts.quiet,
            advise_detach: false,
        },
    )
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let create = RefCell::new(None);
    let force_create = RefCell::new(None);
    let guess = Cell::new(-1);
    let discard = Cell::new(false);
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
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string('c', "create", &create, "branch", msg::HELP_CREATE),
            Opt::string('C', "force-create", &force_create, "branch", msg::HELP_FORCE_CREATE),
            Opt::bool(None, "guess", &guess, msg::HELP_GUESS_SWITCH),
            Opt::flag(None, "discard-changes", &discard, msg::HELP_DISCARD),
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
            Opt::flag(None, "overwrite-ignore", &overwrite_ignore, msg::HELP_OVERWRITE_IGNORE),
            Opt::flag(None, "ignore-other-worktrees", &ignore_other, msg::HELP_IGNORE_OTHER),
        ],
    );
    let mut args = parser.parse(argv)?.args;
    drop(parser);
    let quiet = quiet.get() > 0;
    let force = force.get() || discard.get();
    let track = track.get();
    let mut create = create.borrow().clone();
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
    if track.is_given() && orphan_name.is_some() {
        return Err(Die::Fatal(msg::ORPHAN_WITH_TRACK.to_owned()));
    }
    // `-t`/`--no-track` alone create a branch named after the start point.
    let implied = track.is_given() && create.is_none() && force_create.is_none();
    if implied && !detach.get() {
        create = Some(implied_branch(&args, "c")?);
    }
    if detach.get()
        && (implied || create.is_some() || force_create.is_some() || orphan_name.is_some())
    {
        return Err(Die::Fatal(msg::DETACH_WITH_CREATE.to_owned()));
    }
    if args.len() > 1 {
        return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let auto = AutoSetup::from_config(&repo.config)?;
    if let Some(name) = orphan_name {
        if !args.is_empty() {
            return Err(Die::Fatal(msg::ONE_REFERENCE_EXPECTED.to_owned()));
        }
        return sb::orphan(&repo, &name, true, quiet);
    }
    let revs = Revs::new(ctx, &repo.setup)?;
    let opts = CreateOptions {
        track,
        auto,
        force,
        quiet,
        invalid_start: &msg::invalid_ref,
    };
    if let Some(name) = force_create.or(create.clone()) {
        let reset = create.is_none();
        return create_and_switch(
            &repo,
            &revs,
            NewBranch {
                name,
                start: args.pop(),
                reset,
            },
            &opts,
        );
    }
    let Some(arg) = args.first().cloned().or_else(|| detach.get().then(|| "HEAD".to_owned()))
    else {
        return Err(Die::Fatal(msg::MISSING_BRANCH_OR_COMMIT.to_owned()));
    };
    let name = expand_previous(&repo, &revs, &arg)?;
    let objects = Objects::open(&repo.setup);
    let named = if detach.get() {
        match revs.resolve(&name).and_then(|id| sb::peel_commit(&objects, &id)) {
            Some(id) => Named::Commit(id),
            None => Named::Missing(None),
        }
    } else {
        lookup(
            &repo,
            &revs,
            &objects,
            &name,
            guess_enabled(&repo, guess.get()),
            "switch",
        )?
    };
    match named {
        Named::Branch(b, id) => sb::switch(
            &repo,
            &revs,
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
            &revs,
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
            let tracked = tracking_ref(&revs, &name)?;
            let (kind, shown) = if let Some(r) = tracked.as_deref().and_then(|t| t.strip_prefix("refs/remotes/")) {
                ("remote branch", r.to_owned())
            } else if let Some(t) = tracked.as_deref().and_then(|t| t.strip_prefix("refs/tags/")) {
                ("tag", t.to_owned())
            } else if matches!(store.read_raw(&format!("refs/tags/{name}")), Ok(Some(_))) {
                ("tag", name.clone())
            } else if matches!(store.read_raw(&format!("refs/remotes/{name}")), Ok(Some(_))) {
                ("remote branch", name.clone())
            } else {
                ("commit", name.clone())
            };
            eprintln!("fatal: {}", msg::branch_expected(kind, &shown));
            eprint!("{}", msg::hint_detach_branch_expected());
            Err(Die::Exit(128))
        }
        Named::Missing(Some(remote_ref)) => create_and_switch(
            &repo,
            &revs,
            NewBranch {
                name: name.clone(),
                start: Some(remote_ref),
                reset: false,
            },
            &opts,
        ),
        Named::Missing(None) => Err(Die::Fatal(msg::invalid_ref(&name))),
    }
}
