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

/// A new branch name given as `@{-<n>}`: the branch checked out before.
pub fn expand_new_name(repo: &Repo, name: &str) -> String {
    if name.starts_with("@{-") {
        previous_name(repo, name)
    } else {
        name.to_owned()
    }
}

/// Resolves `name` to a commit; `<a>...<b>` (either side defaulting to
/// `HEAD`) names their merge base when there is exactly one.
pub fn resolve_commit(revs: &Revs, objects: &Objects, name: &str) -> Option<ObjectId> {
    if let Some(id) = revs.resolve(name).and_then(|id| sb::peel_commit(objects, &id)) {
        return Some(id);
    }
    let (a, b) = name.split_once("...")?;
    let side = |s: &str| {
        let s = if s.is_empty() { "HEAD" } else { s };
        revs.resolve(s).and_then(|id| sb::peel_commit(objects, &id))
    };
    let (a, b) = (side(a)?, side(b)?);
    let bases = revs.with_resolver(|r| r.merge_bases(&a, &b)).ok()?;
    match bases.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// "warning: refname '<name>' is ambiguous." when `name` matches more
/// than one ref.
pub fn warn_if_ambiguous(revs: &Revs, name: &str) {
    if let Ok(d) = revs.with_resolver(|r| r.dwim_ref(name)) {
        if d.count > 1 {
            eprintln!("warning: {}", tmsg::refname_ambiguous(name));
        }
    }
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
            warn_if_ambiguous(revs, name);
            return Ok(Named::Branch(name.to_owned(), id));
        }
    }
    if let Some(id) = resolve_commit(revs, objects, name) {
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
    /// `-l`: create the reflog of the new branch.
    pub reflog: bool,
    /// `switch`: refuse while an operation is in progress.
    pub refuse_in_progress: bool,
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
    let nb = NewBranch {
        name: expand_new_name(repo, &nb.name),
        ..nb
    };
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
            let id = resolve_commit(revs, &objects, &s)
                .ok_or_else(|| Die::Fatal((opts.invalid_start)(&s)))?;
            (s, id)
        }
        None => match store.resolve_oid("HEAD").ok().flatten() {
            Some(id) => ("HEAD".to_owned(), id),
            None => {
                // An unborn branch: only HEAD moves.
                return sb::orphan(repo, &nb.name, false, opts.quiet, opts.reflog);
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
                reflog: opts.reflog,
            }),
            force: opts.force,
            quiet: opts.quiet,
            advise_detach: false,
            refuse_in_progress: opts.refuse_in_progress,
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
    let creating = [create.is_some(), force_create.is_some(), orphan_name.is_some()];
    if creating.iter().filter(|&&c| c).count() > 1 {
        return Err(Die::Fatal(msg::CREATE_OPTIONS_SWITCH.to_owned()));
    }
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
        let name = expand_new_name(&repo, &name);
        return sb::orphan(&repo, &name, true, quiet, false);
    }
    let revs = Revs::new(ctx, &repo.setup)?;
    let opts = CreateOptions {
        track,
        auto,
        force,
        quiet,
        reflog: false,
        refuse_in_progress: true,
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
        match resolve_commit(&revs, &objects, &name) {
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
                refuse_in_progress: true,
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
                refuse_in_progress: true,
            },
        ),
        Named::Commit(_) => {
            // How git names what the argument resolved to: a tag or
            // remote-tracking branch by its short name, any other ref by its
            // full name, anything else as a commit.
            let dwim = revs
                .with_resolver(|r| r.dwim_ref(&name))
                .map_err(|e| Die::Fatal(e.to_string()))?;
            let full = (dwim.count >= 1 && !dwim.full_name.is_empty()).then_some(dwim.full_name);
            let text = match full.as_deref() {
                Some(f) if f.starts_with("refs/remotes/") => {
                    msg::branch_expected("remote branch", &f["refs/remotes/".len()..])
                }
                Some(f) if f.starts_with("refs/tags/") => {
                    msg::branch_expected("tag", &f["refs/tags/".len()..])
                }
                Some(f) => msg::branch_expected_ref(f),
                None => msg::branch_expected("commit", &name),
            };
            eprintln!("fatal: {text}");
            let suggest = rgit_messages::Advisor::new(
                &repo.config,
                &rgit_config::SystemEnv::default(),
                false,
            )
            .and_then(|a| a.enabled(rgit_messages::Advice::SuggestDetachingHead))
            .map_err(Die::Fatal)?;
            if suggest {
                eprint!("{}", msg::hint_detach_branch_expected());
            }
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
