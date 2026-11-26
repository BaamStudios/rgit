// SPDX-License-Identifier: MIT
//! `git worktree` (Documentation/git-worktree.adoc, config/worktree.adoc,
//! gitrepository-layout.adoc `worktrees/`): `add`, `list`, `lock`,
//! `unlock`, `move`, `remove`, `prune` and `repair`.
//!
//! A linked worktree is a directory `$GIT_COMMON_DIR/worktrees/<id>` with
//! `gitdir` (the worktree's `.git` file), `commondir` (`../..`), its own
//! `HEAD`, index and logs, and optionally `locked`; the worktree's `.git`
//! file points back to it. `add` registers the worktree, then sets `HEAD`
//! and checks out with rgit's own commands run inside the new worktree
//! (`update-ref`, `symbolic-ref`, `reset --hard`), as git-worktree(1)
//! describes the result. Output and messages follow the `git` binary.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_refs::RefStore;

use crate::messages::worktree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use crate::worktrees::{self, Worktree};

const SUBCOMMANDS: &[&str] = &[
    "add", "list", "lock", "move", "prune", "remove", "repair", "unlock",
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        msg::USAGE,
        SUBCOMMANDS.iter().map(|s| Opt::subcommand(s)).collect(),
    );
    let parsed = parser.parse(argv)?;
    drop(parser);
    let args = parsed.args;
    match parsed.subcommand.as_deref() {
        Some("add") => add(ctx, &args),
        Some("list") => list(ctx, &args),
        Some("lock") => lock(ctx, &args),
        Some("unlock") => unlock(ctx, &args),
        Some("move") => move_worktree(ctx, &args),
        Some("remove") => remove(ctx, &args),
        Some("prune") => prune(ctx, &args),
        Some("repair") => repair(ctx, &args),
        _ => Err(Die::Output(
            Parser::new(msg::USAGE, Vec::new()).error_with_usage(msg::NEED_SUBCOMMAND),
        )),
    }
}

fn fatal(m: String) -> Die {
    Die::Fatal(m)
}

fn strerror(e: &std::io::Error) -> String {
    crate::global::strerror(e)
}

/// `path` with `.` and `..` components resolved lexically.
fn normalize(p: &Path) -> PathBuf {
    rgit_repo::path::normalize(p)
}

/// A path the user gave, made absolute against the directory the command
/// started in.
fn user_path(repo: &Repo, arg: &str) -> PathBuf {
    normalize(&repo.setup.original_cwd.join(arg))
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

/// `target` relative to the directory `base` (both absolute, normalised).
fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<Component> = target.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Whether links are written with relative paths: `--[no-]relative-paths`,
/// else `worktree.useRelativePaths`.
fn use_relative(repo: &Repo, opt: i32) -> bool {
    match opt {
        0 => false,
        1 => true,
        _ => repo
            .config
            .bool("worktree.userelativepaths")
            .ok()
            .flatten()
            .unwrap_or(false),
    }
}

/// Write the two links between the admin directory `admin` and the work
/// tree `wt`: `<admin>/gitdir` and `<wt>/.git`.
fn write_links(admin: &Path, wt: &Path, relative: bool) -> Result<(), Die> {
    let admin = worktrees::real(admin);
    let wt = worktrees::real(wt);
    let dot_git = wt.join(".git");
    let (to_admin, to_wt) = if relative {
        (relative_path(&admin, &wt), relative_path(&dot_git, &admin))
    } else {
        (admin.clone(), dot_git.clone())
    };
    write_file(&dot_git, format!("gitdir: {}\n", to_admin.display()).as_bytes())?;
    write_file(&admin.join("gitdir"), format!("{}\n", to_wt.display()).as_bytes())
}

fn write_file(path: &Path, data: &[u8]) -> Result<(), Die> {
    std::fs::write(path, data)
        .map_err(|e| fatal(format!("could not write '{}': {}", path.display(), strerror(&e))))
}

/// Relative links need `extensions.relativeWorktrees` (and repository
/// format version 1).
fn enable_relative_extension(ctx: &Context, repo: &Repo) -> Result<(), Die> {
    if repo
        .config
        .bool("extensions.relativeworktrees")
        .ok()
        .flatten()
        .unwrap_or(false)
    {
        return Ok(());
    }
    let child = Child::in_repo(ctx, repo)?;
    child.run(&["config", "core.repositoryformatversion", "1"], false)?;
    child.run(&["config", "extensions.relativeWorktrees", "true"], false)?;
    Ok(())
}

/// rgit's own commands, run as children (in the new worktree for `add`).
struct Child {
    exe: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    dir: PathBuf,
}

impl Child {
    fn in_repo(ctx: &Context, repo: &Repo) -> Result<Child, Die> {
        let exe = std::env::current_exe().map_err(|e| fatal(e.to_string()))?;
        let mut env = ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        env.push(("GIT_DIR".into(), repo.setup.absolute_git_dir().into()));
        if let Some(wt) = &repo.setup.work_tree {
            env.push(("GIT_WORK_TREE".into(), repo.setup.cwd.join(wt).into()));
        }
        let dir = repo.setup.cwd.clone();
        Ok(Child { exe, env, dir })
    }

    fn in_worktree(ctx: &Context, admin: &Path, wt: &Path) -> Result<Child, Die> {
        let exe = std::env::current_exe().map_err(|e| fatal(e.to_string()))?;
        let mut env = ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        env.push(("GIT_DIR".into(), admin.into()));
        env.push(("GIT_WORK_TREE".into(), wt.into()));
        Ok(Child {
            exe,
            env,
            dir: wt.to_path_buf(),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.exe);
        c.args(args)
            .current_dir(&self.dir)
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        c
    }

    /// Runs `rgit <args>`; with `quiet` its standard output is dropped.
    /// A failure ends the command (exit code 255).
    fn run(&self, args: &[&str], quiet: bool) -> Result<(), Die> {
        let mut c = self.command(args);
        if quiet {
            c.stdout(Stdio::null());
        }
        let status = c.status().map_err(|e| fatal(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            // git ends with 255 when a command it runs fails.
            Err(Die::Exit(255))
        }
    }

    /// Runs `rgit <args>` and returns its standard output, `None` when it
    /// fails.
    fn output(&self, args: &[&str]) -> Option<Vec<u8>> {
        let out = self.command(args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then_some(out.stdout)
    }
}

fn all_worktrees(repo: &Repo) -> Vec<Worktree> {
    worktrees::list(&repo.setup, worktrees::main_is_bare(&repo.setup, &repo.config))
}

/// The worktree `arg` names: by path, or by a unique suffix of path
/// components (`git-worktree(1)`, `<worktree>`).
fn find_worktree<'a>(repo: &Repo, all: &'a [Worktree], arg: &str) -> Option<&'a Worktree> {
    let abs = worktrees::real(&user_path(repo, arg));
    if let Some(w) = all.iter().find(|w| worktrees::real(&w.path) == abs) {
        return Some(w);
    }
    let suffix = Path::new(arg);
    if suffix.is_absolute() || suffix.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let mut found = all.iter().filter(|w| w.path.ends_with(suffix));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

fn not_a_working_tree(arg: &str) -> Die {
    fatal(msg::not_a_working_tree(arg))
}

// ---------------------------------------------------------------- add

struct AddOptions {
    force: i32,
    new_branch: Option<String>,
    reset_branch: bool,
    orphan: bool,
    detach: bool,
    checkout: bool,
    lock: bool,
    reason: Option<String>,
    quiet: bool,
    track: i32,
    guess_remote: Option<bool>,
    relative: i32,
}

/// The remote-tracking branch for branch `name` if exactly one remote has
/// one (or `checkout.defaultRemote` names one of them); several fail
/// with git's hint.
fn unique_remote_branch(
    ctx: &Context,
    repo: &Repo,
    name: &str,
    path_arg: &str,
) -> Result<Option<String>, Die> {
    let store = crate::refs_support::ref_store(&repo.setup, &repo.config);
    let revs = crate::refs_support::Revs::new(ctx, &repo.setup)?;
    let trepo = crate::tracking::Repo {
        setup: &repo.setup,
        config: &repo.config,
        store: &store,
        revs: &revs,
    };
    match crate::tracking::guess_remote_branch(&trepo, name)? {
        crate::tracking::Guess::None => Ok(None),
        crate::tracking::Guess::Unique(r) => Ok(Some(r)),
        crate::tracking::Guess::Ambiguous(remotes) => {
            let advisor = ctx.advisor(&repo.config)?;
            advisor
                .advise_without_footer(
                    Advice::CheckoutAmbiguousRemoteBranchName,
                    &msg::hint_ambiguous_remote(name, &remotes, path_arg),
                )
                .map_err(fatal)?;
            Err(fatal(crate::messages::tracking::matched_multiple_remotes(
                name,
                remotes.len(),
            )))
        }
    }
}

fn branch_exists(repo: &Repo, name: &str) -> bool {
    repo.refs.resolve(&format!("refs/heads/{name}")).is_some()
}

/// The final component of `path` (git names the branch and the admin
/// directory after it).
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A name for `worktrees/<id>`: the base name, with a number appended
/// while it is taken.
fn new_id(common: &Path, path: &Path) -> String {
    let mut base = sanitize_id(&basename(path));
    if base.is_empty() {
        base = "worktree".into();
    }
    let dir = common.join("worktrees");
    if !dir.join(&base).exists() {
        return base;
    }
    let mut n = 1;
    loop {
        let id = format!("{base}{n}");
        if !dir.join(&id).exists() {
            return id;
        }
        n += 1;
    }
}

/// The base name made a valid ref name component, the way git names the
/// admin directory: characters a ref cannot hold and a leading dot
/// become `-`, runs of dots collapse, `@{` loses its brace and trailing
/// `.lock` goes.
fn sanitize_id(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let prev = out.chars().last();
        let bad = c.is_ascii_control() || " ~^:?*[\\".contains(c);
        if bad || (c == '.' && prev.is_none()) || (c == '{' && prev == Some('@')) {
            out.push('-');
        } else if c == '.' && prev == Some('.') {
            continue;
        } else {
            out.push(c);
        }
    }
    while let Some(stripped) = out.strip_suffix(".lock") {
        out = stripped.to_string();
    }
    out
}

/// Refuses a path that exists (unless it is an empty directory) or that
/// belongs to a registered worktree which is missing (unless forced).
fn check_target(
    all: &[Worktree],
    path: &Path,
    arg: &str,
    force: i32,
    cmd: &str,
) -> Result<Option<PathBuf>, Die> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let empty_dir = meta.is_dir()
            && std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_none());
        if !empty_dir {
            return Err(fatal(msg::already_exists(arg)));
        }
    }
    let real = worktrees::real(path);
    let Some(wt) = all
        .iter()
        .find(|w| !w.is_main() && worktrees::real(&w.path) == real)
    else {
        return Ok(None);
    };
    let locked = wt.lock_reason().is_some();
    if locked && force < 2 {
        return Err(fatal(msg::missing_locked(arg, cmd)));
    }
    if !locked && force < 1 {
        return Err(fatal(msg::missing_registered(arg, cmd)));
    }
    Ok(Some(wt.git_dir.clone()))
}

fn add(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let force = Cell::new(0);
    let new_b: RefCell<Option<String>> = RefCell::new(None);
    let reset_b: RefCell<Option<String>> = RefCell::new(None);
    let orphan = Cell::new(false);
    let detach = Cell::new(false);
    let checkout = Cell::new(1);
    let lock_opt = Cell::new(false);
    let reason: RefCell<Option<String>> = RefCell::new(None);
    let quiet = Cell::new(false);
    let track = Cell::new(-1);
    let guess = Cell::new(-1);
    let relative = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_ADD,
        vec![
            Opt::force(&force, msg::HELP_ADD_FORCE),
            Opt::string('b', None, &new_b, msg::ARGH_BRANCH, msg::HELP_NEW_BRANCH),
            Opt::string('B', None, &reset_b, msg::ARGH_BRANCH, msg::HELP_RESET_BRANCH),
            Opt::flag(None, "orphan", &orphan, msg::HELP_ORPHAN),
            Opt::flag('d', "detach", &detach, msg::HELP_DETACH),
            Opt::bool(None, "checkout", &checkout, msg::HELP_CHECKOUT),
            Opt::flag(None, "lock", &lock_opt, msg::HELP_LOCK),
            Opt::string(None, "reason", &reason, msg::ARGH_STRING, msg::HELP_REASON),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::bool(None, "track", &track, msg::HELP_TRACK),
            Opt::bool(None, "guess-remote", &guess, msg::HELP_GUESS_REMOTE),
            Opt::bool(None, "relative-paths", &relative, msg::HELP_RELATIVE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() || args.len() > 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let b = new_b.take();
    let bb = reset_b.take();
    let given = [b.is_some(), bb.is_some(), detach.get()];
    if given.iter().filter(|x| **x).count() > 1 {
        return Err(fatal(msg::BRANCH_DETACH_EXCLUSIVE.into()));
    }
    if orphan.get() && detach.get() {
        return Err(fatal(msg::ORPHAN_DETACH.into()));
    }
    if orphan.get() && checkout.get() == 0 {
        return Err(fatal(msg::ORPHAN_NO_CHECKOUT.into()));
    }
    if orphan.get() && args.len() > 1 {
        return Err(fatal(msg::ORPHAN_COMMITISH.into()));
    }
    let reason = reason.take();
    if reason.is_some() && !lock_opt.get() {
        return Err(fatal(msg::REASON_NEEDS_LOCK.into()));
    }
    let opts = AddOptions {
        force: force.get(),
        reset_branch: bb.is_some(),
        new_branch: b.or(bb),
        orphan: orphan.get(),
        detach: detach.get(),
        checkout: checkout.get() != 0,
        lock: lock_opt.get(),
        reason,
        quiet: quiet.get(),
        track: track.get(),
        guess_remote: match guess.get() {
            -1 => None,
            v => Some(v != 0),
        },
        relative: relative.get(),
    };
    let mut repo = Repo::open(ctx, Mode::Required)?;
    add_worktree(ctx, &mut repo, &args[0], args.get(1).map(String::as_str), opts)
}

/// What the new worktree's `HEAD` becomes.
enum Head {
    /// An existing branch (short name).
    Branch(String),
    /// A branch to create (or reset) at `start` (`None`: unborn).
    NewBranch {
        name: String,
        start: Option<String>,
        track: bool,
    },
    /// Detached at a commit.
    Detached,
}

fn add_worktree(
    ctx: &Context,
    repo: &mut Repo,
    path_arg: &str,
    commitish: Option<&str>,
    mut opts: AddOptions,
) -> CmdResult {
    let path = user_path(repo, path_arg);
    // `-` is `@{-1}`, the branch checked out before.
    let commitish = match commitish {
        Some("-") | Some("@{-1}") => Some(
            crate::switch_branch::previous_checkout(repo, 1)
                .ok_or_else(|| fatal(msg::invalid_reference("@{-1}")))?,
        ),
        c => c.map(str::to_string),
    };
    let guess_remote = opts.guess_remote.unwrap_or_else(|| {
        repo.config
            .bool("worktree.guessremote")
            .ok()
            .flatten()
            .unwrap_or(false)
    });

    // Without a commit to start from, an unborn branch is inferred
    // (`--orphan`), or a bad HEAD is reported.
    if !opts.orphan && commitish.is_none() && !opts.detach {
        let any = |prefix: &str| {
            crate::refs_support::ref_store(&repo.setup, &repo.config)
                .iter(&rgit_refs::IterOptions::prefix(prefix))
                .map(|v| v.iter().any(|i| matches!(i, rgit_refs::IterItem::Ref(_))))
                .unwrap_or(false)
        };
        if !any("refs/heads/") {
            let dwim = opts.new_branch.is_none();
            let remote_refs = any("refs/remotes/");
            let has_remote = !repo.config.subsections("remote").is_empty();
            if dwim && guess_remote && has_remote && !remote_refs && opts.force == 0 {
                return Err(fatal(msg::NEED_FETCH.into()));
            }
            if !(dwim && guess_remote && remote_refs) {
                if !opts.quiet {
                    eprintln!("{}", msg::INFER_ORPHAN);
                }
                if !opts.checkout {
                    return Err(fatal(msg::ORPHAN_NO_CHECKOUT.into()));
                }
                if opts.track == 1 {
                    return Err(fatal(msg::ORPHAN_TRACK.into()));
                }
                opts.orphan = true;
            }
        }
    }
    // A remote-tracking branch guessed from the path needs no HEAD.
    let guessed = !opts.orphan
        && commitish.is_none()
        && opts.new_branch.is_none()
        && !opts.detach
        && guess_remote
        && repo.resolve("HEAD").is_none()
        && !branch_exists(repo, &basename(&path))
        && unique_remote_branch(ctx, repo, &basename(&path), path_arg)?.is_some();
    if !opts.orphan && commitish.is_none() && !guessed && repo.resolve("HEAD").is_none() {
        if !opts.quiet {
            eprintln!("warning: {}", msg::BAD_HEAD);
            eprintln!();
            let mut example = String::from("--orphan ");
            if let Some(b) = &opts.new_branch {
                example.push_str(&format!("-b {b} "));
            }
            example.push_str(path_arg);
            let advisor = ctx.advisor(&repo.config)?;
            advisor
                .advise(Advice::WorktreeAddOrphan, &msg::orphan_hint(&example))
                .map_err(fatal)?;
        }
        return Err(fatal(msg::invalid_reference("HEAD")));
    }

    // Decide what HEAD becomes.
    let mut implicit_track = false;
    let mut commitish = commitish;
    let head = if let Some(name) = opts.new_branch.clone() {
        Head::NewBranch {
            name,
            start: if opts.orphan { None } else { commitish.clone() },
            track: false,
        }
    } else if opts.orphan {
        Head::NewBranch {
            name: basename(&path),
            start: None,
            track: false,
        }
    } else if opts.detach {
        Head::Detached
    } else if let Some(c) = commitish.clone() {
        if branch_exists(repo, &c) {
            Head::Branch(c)
        } else if repo.resolve(&c).is_none() {
            match unique_remote_branch(ctx, repo, &c, path_arg)? {
                Some(remote) => {
                    implicit_track = true;
                    // The full name (the branch reflog names it).
                    commitish = Some(remote.clone());
                    Head::NewBranch {
                        name: c,
                        start: commitish.clone(),
                        track: true,
                    }
                }
                None => Head::Detached,
            }
        } else {
            Head::Detached
        }
    } else {
        let name = basename(&path);
        if branch_exists(repo, &name) {
            commitish = Some(name.clone());
            Head::Branch(name)
        } else {
            let remote = if guess_remote {
                unique_remote_branch(ctx, repo, &name, path_arg)?
            } else {
                None
            };
            match remote {
                Some(r) => {
                    implicit_track = true;
                    commitish = Some(r.clone());
                    Head::NewBranch {
                        name,
                        start: commitish.clone(),
                        track: true,
                    }
                }
                None => Head::NewBranch {
                    name,
                    start: None,
                    track: false,
                },
            }
        }
    };

    // The commit to check out.
    let commit = if opts.orphan {
        None
    } else {
        let spec = commitish.clone().unwrap_or_else(|| "HEAD".into());
        let id = repo
            .resolve(&spec)
            .and_then(|id| repo.peel_to(id, ObjectKind::Commit));
        match id {
            Some(id) => Some(id),
            None => return Err(fatal(msg::invalid_reference(&spec))),
        }
    };

    let all = all_worktrees(repo);
    let in_use = |name: &str| worktrees::checked_out(&all, &format!("refs/heads/{name}"), true);
    // -B refuses a branch checked out anywhere before anything else.
    if let (Head::NewBranch { name, .. }, true) = (&head, opts.reset_branch) {
        if branch_exists(repo, name) && opts.force == 0 {
            if let Some(w) = in_use(name) {
                return Err(fatal(msg::already_used_by(name, &display(&w.path))));
            }
        }
    }

    if !opts.quiet {
        let line = match &head {
            Head::NewBranch { name, .. } => {
                let old = repo.refs.resolve(&format!("refs/heads/{name}"));
                match old {
                    Some(old) if opts.reset_branch => {
                        msg::preparing_reset_branch(name, &repo.abbrev(&old, repo.default_abbrev()))
                    }
                    _ => msg::preparing_new_branch(name),
                }
            }
            Head::Branch(name) => msg::preparing_checkout(name),
            Head::Detached => {
                let id = commit.unwrap_or_else(|| ObjectId::null(repo.hash));
                msg::preparing_detached(&repo.abbrev(&id, repo.default_abbrev()))
            }
        };
        eprintln!("{line}");
    }

    // Create (or reset) the branch.
    let parent = Child::in_repo(ctx, repo)?;
    if let Head::NewBranch { name, start, track } = &head {
        if !opts.orphan {
            let mut args = vec!["branch"];
            if opts.reset_branch {
                args.push("--force");
            }
            match opts.track {
                1 => args.push("--track"),
                0 => args.push("--no-track"),
                _ if *track && implicit_track => args.push("--track"),
                _ => {}
            }
            if opts.quiet {
                args.push("--quiet");
            }
            args.push(name);
            args.push(start.as_deref().unwrap_or("HEAD"));
            parent.run(&args, false)?;
        } else if branch_exists(repo, name) {
            return Err(fatal(crate::messages::branch::already_exists(name)));
        }
    }

    // git creates the branch before it looks at the path.
    let reuse = check_target(&all, &path, path_arg, opts.force, "add")?;
    if let Head::Branch(name) = &head {
        if opts.force == 0 {
            if let Some(w) = in_use(name) {
                return Err(fatal(msg::already_used_by(name, &display(&w.path))));
            }
        }
    }

    // Register the worktree.
    if let Some(admin) = reuse {
        let _ = std::fs::remove_dir_all(admin);
    }
    let common = repo.setup.absolute_common_dir();
    let id = new_id(&common, &path);
    let admin = common.join("worktrees").join(&id);
    std::fs::create_dir_all(&path)
        .map_err(|e| fatal(msg::could_not_create_dir(path_arg, &strerror(&e))))?;
    std::fs::create_dir_all(admin.join("refs"))
        .map_err(|e| fatal(msg::could_not_create_dir(&display(&admin), &strerror(&e))))?;
    let lock_text = if opts.lock {
        match &opts.reason {
            Some(r) => format!("{r}\n"),
            None => format!("{}\n", msg::LOCKED_BY_ADD),
        }
    } else {
        msg::INITIALIZING.to_string()
    };
    write_file(&admin.join("locked"), lock_text.as_bytes())?;
    let relative = use_relative(repo, opts.relative);
    write_links(&admin, &path, relative)?;
    write_file(&admin.join("commondir"), b"../..\n")?;
    let null = ObjectId::null(repo.hash).to_hex();
    if repo.setup.format.ref_storage == rgit_repo::RefStorage::Reftable {
        // The worktree gets a stack of its own for its per-worktree
        // references; `HEAD` lives there, not in the file.
        let shared = crate::refs_support::shared_repository(&repo.config);
        let made = rgit_refs::reftable::backend::create(&admin, repo.hash, shared).and_then(|()| {
            match (&head, opts.orphan) {
                (Head::NewBranch { name, .. }, true) => {
                    rgit_refs::write_head_symref(&admin, &format!("refs/heads/{name}")).map(|_| ())
                }
                _ => Ok(()),
            }
        });
        made.map_err(|e| fatal(msg::could_not_create_dir(&display(&admin), &strerror(&e))))?;
    } else {
        match (&head, opts.orphan) {
            (Head::NewBranch { name, .. }, true) => {
                write_file(&admin.join("HEAD"), format!("ref: refs/heads/{name}\n").as_bytes())?;
            }
            _ => write_file(&admin.join("HEAD"), format!("{null}\n").as_bytes())?,
        }
    }
    if relative {
        enable_relative_extension(ctx, repo)?;
    }
    copy_worktree_config(ctx, repo, &common, &admin)?;
    copy_sparse_checkout(repo, &admin)?;

    let admin_real = worktrees::real(&admin);
    let wt_real = worktrees::real(&path);
    let child = Child::in_worktree(ctx, &admin_real, &wt_real)?;
    let result = (|| -> Result<(), Die> {
        if opts.orphan {
            return child.run(&["read-tree", "--empty"], true);
        }
        let commit = commit.expect("commit for non-orphan worktree");
        let hex = commit.to_hex();
        // One update, so that `HEAD` gets one reflog entry.
        match &head {
            Head::Branch(name) | Head::NewBranch { name, .. } => {
                child.run(&["symbolic-ref", "HEAD", &format!("refs/heads/{name}")], true)?;
            }
            _ => child.run(&["update-ref", "--no-deref", "HEAD", &hex], true)?,
        }
        if opts.checkout {
            let mut args = vec!["reset", "--hard"];
            if opts.quiet {
                args.push("--quiet");
            }
            child.run(&args, false)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        // Leave nothing half-made behind.
        let _ = std::fs::remove_dir_all(&admin);
        let _ = std::fs::remove_file(path.join(".git"));
        let _ = std::fs::remove_dir(&path);
        prune_worktrees_dir(&common);
        return Err(e);
    }
    if !opts.lock {
        let _ = std::fs::remove_file(admin.join("locked"));
    }
    if opts.checkout && !opts.orphan {
        run_post_checkout(ctx, repo, &admin_real, &wt_real, commit)?;
    }
    Ok(0)
}

/// With `extensions.worktreeConfig`, the new worktree starts with the main
/// worktree's `config.worktree`, without `core.bare` and `core.worktree`.
fn copy_worktree_config(ctx: &Context, repo: &Repo, common: &Path, admin: &Path) -> Result<(), Die> {
    if !repo.setup.format.worktree_config {
        return Ok(());
    }
    let src = common.join("config.worktree");
    let Ok(data) = std::fs::read(&src) else {
        return Ok(());
    };
    let dst = admin.join("config.worktree");
    write_file(&dst, &data)?;
    let child = Child::in_repo(ctx, repo)?;
    let file = dst.display().to_string();
    for key in ["core.bare", "core.worktree"] {
        // Not set is fine.
        let _ = child.output(&["config", "--file", &file, "--unset-all", key]);
    }
    Ok(())
}

/// In a sparse checkout the new worktree starts with the current
/// worktree's sparse-checkout patterns (git-sparse-checkout(1), `set`).
fn copy_sparse_checkout(repo: &Repo, admin: &Path) -> Result<(), Die> {
    if !crate::sparse::enabled(&repo.config) {
        return Ok(());
    }
    let Ok(data) = std::fs::read(crate::sparse::file_path(repo)) else {
        return Ok(());
    };
    let info = admin.join("info");
    std::fs::create_dir_all(&info).map_err(|e| Die::Fatal(e.to_string()))?;
    write_file(&info.join("sparse-checkout"), &data)
}

/// `post-checkout` with `<null> <new> 1`, run in the new worktree.
fn run_post_checkout(
    ctx: &Context,
    repo: &Repo,
    admin: &Path,
    wt: &Path,
    commit: Option<ObjectId>,
) -> Result<(), Die> {
    let null = ObjectId::null(repo.hash);
    let mut setup = repo.setup.clone();
    setup.common_dir = repo.setup.absolute_common_dir();
    setup.git_dir = admin.to_path_buf();
    setup.work_tree = Some(wt.to_path_buf());
    setup.cwd = wt.to_path_buf();
    setup.prefix = None;
    let hc = crate::hooks::HookContext::new(ctx);
    let hooks = crate::hooks::with_context(&hc, &setup, &repo.config)?;
    let args = [
        null.to_hex(),
        commit.unwrap_or(null).to_hex(),
        "1".to_owned(),
    ];
    let opts = rgit_hook::RunOptions::with_args(args);
    crate::hooks::run(&hooks, "post-checkout", &opts)?;
    Ok(())
}

/// Removes `worktrees/` when it is empty.
fn prune_worktrees_dir(common: &Path) {
    let _ = std::fs::remove_dir(common.join("worktrees"));
}

// --------------------------------------------------------------- list

fn list(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let porcelain = Cell::new(false);
    let verbose = Cell::new(0);
    let expire: RefCell<Option<String>> = RefCell::new(None);
    let nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_LIST,
        vec![
            Opt::flag(None, "porcelain", &porcelain, msg::HELP_PORCELAIN),
            Opt::verbose(&verbose, msg::HELP_LIST_VERBOSE),
            Opt::string(None, "expire", &expire, msg::ARGH_EXPIRE, msg::HELP_LIST_EXPIRE),
            Opt::flag('z', None, &nul, msg::HELP_Z).noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    if verbose.get() > 0 && porcelain.get() {
        return Err(fatal(msg::VERBOSE_PORCELAIN.into()));
    }
    if nul.get() && !porcelain.get() {
        return Err(fatal(msg::Z_NEEDS_PORCELAIN.into()));
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    let expire = parse_expire(expire.take().as_deref())?;
    let all = all_worktrees(&repo);
    let mut out = Vec::new();
    if porcelain.get() {
        let end = if nul.get() { b'\0' } else { b'\n' };
        for w in &all {
            out.extend_from_slice(format!("worktree {}", display(&w.path)).as_bytes());
            out.push(end);
            if w.bare {
                out.extend_from_slice(b"bare");
                out.push(end);
            } else {
                let head = w.head.unwrap_or_else(|| ObjectId::null(repo.hash));
                out.extend_from_slice(format!("HEAD {}", head.to_hex()).as_bytes());
                out.push(end);
                if !w.head_broken {
                    match &w.head_ref {
                        Some(r) => out.extend_from_slice(format!("branch {r}").as_bytes()),
                        None => out.extend_from_slice(b"detached"),
                    }
                    out.push(end);
                }
            }
            if let Some(reason) = w.lock_reason() {
                out.extend_from_slice(b"locked");
                if !reason.is_empty() {
                    out.push(b' ');
                    if nul.get() {
                        out.extend_from_slice(reason.as_bytes());
                    } else {
                        crate::quote::write_quoted(&mut out, reason.as_bytes(), false);
                    }
                }
                out.push(end);
            }
            if let Some(reason) = prunable(w, expire) {
                out.extend_from_slice(format!("prunable {reason}").as_bytes());
                out.push(end);
            }
            out.push(end);
        }
    } else {
        let quote_high = repo.quote_path();
        let shown: Vec<String> = all
            .iter()
            .map(|w| {
                let mut b = Vec::new();
                crate::quote::write_quoted(&mut b, w.path.as_os_str().as_encoded_bytes(), quote_high);
                String::from_utf8_lossy(&b).into_owned()
            })
            .collect();
        let width = shown.iter().map(|s| s.chars().count()).max().unwrap_or(0);
        let abbrev = repo.default_abbrev();
        for (w, path) in all.iter().zip(&shown) {
            let pad = " ".repeat(width - path.chars().count());
            let mut line = format!("{path}{pad} ");
            if w.bare {
                line.push_str("(bare)");
            } else {
                let head = w.head.unwrap_or_else(|| ObjectId::null(repo.hash));
                line.push_str(&repo.abbrev(&head, abbrev));
                line.push(' ');
                if w.head_broken {
                    line.push_str("(error)");
                } else {
                    match &w.head_ref {
                        Some(r) => {
                            line.push('[');
                            line.push_str(r.strip_prefix("refs/heads/").unwrap_or(r));
                            line.push(']');
                        }
                        None => line.push_str("(detached HEAD)"),
                    }
                }
            }
            let mut extra = String::new();
            if let Some(reason) = w.lock_reason() {
                if verbose.get() > 0 && !reason.is_empty() {
                    extra.push_str(&format!("\n\tlocked: {reason}"));
                } else {
                    line.push_str(" locked");
                }
            }
            if let Some(reason) = prunable(w, expire) {
                if verbose.get() > 0 {
                    extra.push_str(&format!("\n\tprunable: {reason}"));
                } else {
                    line.push_str(" prunable");
                }
            }
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(extra.as_bytes());
            out.push(b'\n');
        }
    }
    let _ = std::io::stdout().write_all(&out);
    Ok(0)
}

fn prunable(w: &Worktree, expire: Option<u64>) -> Option<&'static str> {
    if w.is_main() {
        return None;
    }
    worktrees::prune_reason(&w.git_dir, expire.map(to_system_time))
}

fn to_system_time(secs: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(secs))
        .unwrap_or_else(|| std::time::UNIX_EPOCH + std::time::Duration::from_secs(i64::MAX as u64 / 2))
}

/// `--expire <time>`: `None` when not given.
fn parse_expire(v: Option<&str>) -> Result<Option<u64>, Die> {
    let Some(v) = v else { return Ok(None) };
    rgit_date::parse_expiry_date(v, &rgit_date::DateContext::from_env())
        .map(Some)
        .map_err(|e| fatal(e.to_string()))
}

// ------------------------------------------------------- lock / unlock

fn lock(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let reason: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE_LOCK,
        vec![Opt::string(None, "reason", &reason, msg::ARGH_STRING, msg::HELP_REASON)],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let all = all_worktrees(&repo);
    let arg = &args[0];
    let w = find_worktree(&repo, &all, arg).ok_or_else(|| not_a_working_tree(arg))?;
    if w.is_main() {
        return Err(fatal(msg::MAIN_LOCK.into()));
    }
    if let Some(old) = w.lock_reason() {
        return Err(fatal(if old.is_empty() {
            msg::already_locked(arg)
        } else {
            msg::already_locked_reason(arg, &old)
        }));
    }
    let text = match reason.take() {
        Some(r) => format!("{r}\n"),
        None => String::new(),
    };
    write_file(&w.git_dir.join("locked"), text.as_bytes())?;
    Ok(0)
}

fn unlock(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(msg::USAGE_UNLOCK, Vec::new());
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let all = all_worktrees(&repo);
    let arg = &args[0];
    let w = find_worktree(&repo, &all, arg).ok_or_else(|| not_a_working_tree(arg))?;
    if w.is_main() {
        return Err(fatal(msg::MAIN_LOCK.into()));
    }
    if w.lock_reason().is_none() {
        return Err(fatal(msg::not_locked(arg)));
    }
    std::fs::remove_file(w.git_dir.join("locked")).map_err(|e| fatal(strerror(&e)))?;
    Ok(0)
}

// --------------------------------------------------------------- move

/// Whether the worktree's links are relative (its `.git` file).
fn links_are_relative(w: &Worktree) -> bool {
    std::fs::read_to_string(w.git_dir.join("gitdir"))
        .map(|s| !Path::new(s.trim_end()).is_absolute())
        .unwrap_or(false)
}

fn move_worktree(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let force = Cell::new(0);
    let relative = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_MOVE,
        vec![
            Opt::force(&force, msg::HELP_MOVE_FORCE),
            Opt::bool(None, "relative-paths", &relative, msg::HELP_RELATIVE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let all = all_worktrees(&repo);
    let (arg, dst_arg) = (&args[0], &args[1]);
    let w = find_worktree(&repo, &all, arg).ok_or_else(|| not_a_working_tree(arg))?;
    if w.is_main() {
        return Err(fatal(msg::is_main(arg)));
    }
    let mut dst = user_path(&repo, dst_arg);
    let mut dst_shown = dst_arg.clone();
    if dst.is_dir() {
        let name = basename(&w.path);
        dst = dst.join(&name);
        dst_shown = format!("{}/{name}", dst_arg.trim_end_matches('/'));
    }
    let reuse = check_move_target(&all, &dst, dst_arg, &dst_shown, force.get())?;
    if let Some(reason) = w.lock_reason() {
        if force.get() < 2 {
            return Err(fatal(msg::cannot_locked("move", &reason)));
        }
    }
    // Submodule repositories of the worktree live in its admin directory.
    if w.git_dir.join("modules").exists() {
        return Err(fatal(msg::CONTAINS_SUBMODULES.into()));
    }
    if let Some(admin) = reuse {
        let _ = std::fs::remove_dir_all(admin);
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::rename(&w.path, &dst).map_err(|e| {
        fatal(msg::failed_to_move(&display(&w.path), &dst_shown, &strerror(&e)))
    })?;
    let rel = match relative.get() {
        -1 => {
            links_are_relative(w)
                || repo
                    .config
                    .bool("worktree.userelativepaths")
                    .ok()
                    .flatten()
                    .unwrap_or(false)
        }
        v => v != 0,
    };
    write_links(&w.git_dir, &dst, rel)?;
    if rel {
        enable_relative_extension(ctx, &repo)?;
    }
    Ok(0)
}

fn check_move_target(
    all: &[Worktree],
    dst: &Path,
    arg: &str,
    shown: &str,
    force: i32,
) -> Result<Option<PathBuf>, Die> {
    if dst.exists() {
        let empty_dir = dst.is_dir() && std::fs::read_dir(dst).is_ok_and(|mut d| d.next().is_none());
        if !empty_dir {
            return Err(fatal(msg::target_exists(shown)));
        }
        return Ok(None);
    }
    check_target(all, dst, arg, force, "move")
}

// ------------------------------------------------------------- remove

fn remove(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let force = Cell::new(0);
    let mut parser = Parser::new(
        msg::USAGE_REMOVE,
        vec![Opt::force(&force, msg::HELP_REMOVE_FORCE)],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let all = all_worktrees(&repo);
    let arg = &args[0];
    let w = find_worktree(&repo, &all, arg).ok_or_else(|| not_a_working_tree(arg))?;
    if w.is_main() {
        return Err(fatal(msg::is_main(arg)));
    }
    if let Some(reason) = w.lock_reason() {
        if force.get() < 2 {
            return Err(fatal(msg::cannot_locked("remove", &reason)));
        }
    }
    if force.get() == 0 && w.git_dir.join("modules").exists() {
        return Err(fatal(msg::CONTAINS_SUBMODULES.into()));
    }
    let present = w.path.join(".git").exists();
    if present && force.get() == 0 {
        let child = Child::in_worktree(ctx, &w.git_dir, &w.path)?;
        let out = child
            .output(&["status", "--porcelain"])
            .ok_or_else(|| fatal(msg::status_failed(&display(&w.path))))?;
        if !out.is_empty() {
            return Err(fatal(msg::dirty(arg)));
        }
    }
    let mut code = 0;
    if w.path.exists() {
        if let Err(e) = std::fs::remove_dir_all(&w.path) {
            eprintln!(
                "error: {}",
                msg::failed_to_delete(&display(&w.path), &strerror(&e))
            );
            code = 1;
        }
    }
    if let Err(e) = std::fs::remove_dir_all(&w.git_dir) {
        eprintln!(
            "error: {}",
            msg::failed_to_delete(&display(&w.git_dir), &strerror(&e))
        );
        code = 1;
    }
    prune_worktrees_dir(&repo.setup.absolute_common_dir());
    Ok(code)
}

// -------------------------------------------------------------- prune

fn prune(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(false);
    let verbose = Cell::new(0);
    let expire: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE_PRUNE,
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_PRUNE_DRY_RUN),
            Opt::verbose(&verbose, msg::HELP_PRUNE_VERBOSE),
            Opt::string(None, "expire", &expire, msg::ARGH_EXPIRE, msg::HELP_PRUNE_EXPIRE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let expire = parse_expire(expire.take().as_deref())?;
    let show = dry_run.get() || verbose.get() > 0;
    let common = repo.setup.absolute_common_dir();
    let mut seen = vec![worktrees::real(&worktrees::main_path(&common))];
    // In directory order, like git.
    for id in worktrees::linked_ids_unsorted(&common) {
        let admin = common.join("worktrees").join(&id);
        let mut reason = worktrees::prune_reason(&admin, expire.map(to_system_time));
        if reason.is_none() && !admin.join("locked").exists() {
            if let Some(target) = worktrees::gitdir_target(&admin) {
                let path = worktrees::real(&worktrees::main_path(&target));
                if seen.contains(&path) {
                    reason = Some(msg::PRUNE_DUPLICATE);
                } else {
                    seen.push(path);
                }
            }
        }
        let Some(reason) = reason else { continue };
        if show {
            eprintln!("{}", msg::removing(&id, reason));
        }
        if !dry_run.get() {
            if admin.is_dir() {
                let _ = std::fs::remove_dir_all(&admin);
            } else {
                let _ = std::fs::remove_file(&admin);
            }
        }
    }
    if !dry_run.get() {
        prune_worktrees_dir(&common);
    }
    Ok(0)
}

// ------------------------------------------------------------- repair

/// What a worktree's `.git` file says.
enum DotGit {
    /// No `.git`, or not a `gitdir:` file.
    Broken,
    /// `.git` is a directory (a repository of its own).
    NotAFile,
    /// The admin directory it names (absolute, normalised).
    Points(PathBuf),
}

fn read_dot_git(wt: &Path) -> DotGit {
    let file = wt.join(".git");
    match std::fs::metadata(&file) {
        Ok(m) if m.is_dir() => return DotGit::NotAFile,
        Ok(_) => {}
        Err(_) => return DotGit::Broken,
    }
    let Ok(data) = std::fs::read(&file) else {
        return DotGit::Broken;
    };
    let text = String::from_utf8_lossy(&data);
    match text
        .strip_prefix("gitdir: ")
        .map(|t| t.trim_end_matches(['\n', '\r']))
        .filter(|t| !t.is_empty())
    {
        Some(t) => DotGit::Points(normalize(&wt.join(t))),
        None => DotGit::Broken,
    }
}

/// Whether the links of `admin` are of the other kind (relative or
/// absolute) than `relative` asks for.
fn gitdir_is_relative(admin: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(admin.join("gitdir")).ok()?;
    Some(!Path::new(text.trim_end()).is_absolute())
}

fn dot_git_is_relative(wt: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(wt.join(".git")).ok()?;
    let t = text.strip_prefix("gitdir: ")?;
    Some(!Path::new(t.trim_end()).is_absolute())
}

fn repair_report(what: &str, path: &Path) {
    eprintln!("{}", msg::repair_line(what, &display(path)));
}

fn repair_error(what: &str, path: &str) {
    eprintln!("error: {}", msg::repair_error(what, path));
}

fn repair(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let relative_opt = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_REPAIR,
        vec![Opt::bool(None, "relative-paths", &relative_opt, msg::HELP_RELATIVE)],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let common = worktrees::real(&repo.setup.absolute_common_dir());
    let admin_root = common.join("worktrees");
    let relative = use_relative(&repo, relative_opt.get());
    let mut failed = false;

    // From the worktrees to the repository: the paths given, else the
    // current linked worktree.
    let mut paths: Vec<(PathBuf, String)> = Vec::new();
    if args.is_empty() {
        if repo.setup.git_dir != repo.setup.common_dir {
            if let Some(wt) = &repo.setup.work_tree {
                let p = repo.setup.cwd.join(wt);
                let shown = display(&p);
                paths.push((p, shown));
            }
        }
    } else {
        for a in &args {
            paths.push((user_path(&repo, a), a.clone()));
        }
    }
    for (wt, shown) in paths {
        if !wt.is_dir() {
            repair_error(msg::REPAIR_NOT_VALID_PATH, &shown);
            failed = true;
            continue;
        }
        let wt = worktrees::real(&wt);
        let named = match read_dot_git(&wt) {
            DotGit::NotAFile => {
                repair_error(msg::REPAIR_NOT_A_FILE, &display(&wt));
                failed = true;
                continue;
            }
            DotGit::Broken => {
                repair_error(msg::REPAIR_DOTGIT_BROKEN, &display(&wt));
                failed = true;
                continue;
            }
            DotGit::Points(p) => p,
        };
        // The admin directory: the one named when it belongs to this
        // repository, else ours of the same name (the repository moved
        // or was copied).
        let own = worktrees::real(&named);
        let admin = if own.parent() == Some(admin_root.as_path()) && own.is_dir() {
            own
        } else {
            let guess = admin_root.join(named.file_name().unwrap_or_default());
            if !guess.is_dir() {
                repair_error(msg::REPAIR_NOT_A_REPO, &display(&wt));
                failed = true;
                continue;
            }
            guess
        };
        let expected = wt.join(".git");
        let what = match worktrees::gitdir_target(&admin) {
            None => Some(msg::REPAIR_GITDIR_UNREADABLE),
            Some(t) if worktrees::real(&t) != expected => Some(msg::REPAIR_GITDIR_INCORRECT),
            Some(_) if gitdir_is_relative(&admin) != Some(relative) => {
                Some(msg::REPAIR_GITDIR_MISMATCH)
            }
            Some(_) => None,
        };
        if let Some(w) = what {
            repair_report(w, &admin.join("gitdir"));
        }
        let dot_git_ok = worktrees::real(&named) == admin && dot_git_is_relative(&wt) == Some(relative);
        if what.is_some() || !dot_git_ok {
            write_links(&admin, &wt, relative)?;
        }
    }

    // From the repository to the worktrees: every `.git` file must point
    // back to its admin directory.
    for id in worktrees::linked_ids(&common) {
        let admin = admin_root.join(&id);
        let Some(target) = worktrees::gitdir_target(&admin) else {
            continue;
        };
        let wt = worktrees::main_path(&target);
        if !wt.exists() {
            continue;
        }
        if !wt.is_dir() {
            repair_error(msg::REPAIR_NOT_A_DIR, &display(&wt));
            failed = true;
            continue;
        }
        let wt = worktrees::real(&wt);
        let what = match read_dot_git(&wt) {
            DotGit::NotAFile => {
                repair_error(msg::REPAIR_NOT_A_FILE, &display(&wt));
                failed = true;
                continue;
            }
            DotGit::Broken => Some(msg::REPAIR_DOTGIT_BROKEN),
            DotGit::Points(p) if worktrees::real(&p) == admin => {
                (dot_git_is_relative(&wt) != Some(relative)).then_some(msg::REPAIR_DOTGIT_MISMATCH)
            }
            DotGit::Points(p) if p.exists() => Some(msg::REPAIR_DOTGIT_INCORRECT),
            DotGit::Points(_) => Some(msg::REPAIR_DOTGIT_BROKEN),
        };
        let gitdir_mismatch = gitdir_is_relative(&admin) != Some(relative);
        if let Some(w) = what {
            repair_report(w, &wt);
        }
        if what.is_some() || gitdir_mismatch {
            write_links(&admin, &wt, relative)?;
        }
    }
    if relative {
        enable_relative_extension(ctx, &repo)?;
    }
    Ok(i32::from(failed))
}
