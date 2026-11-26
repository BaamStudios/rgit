// SPDX-License-Identifier: MIT
//! `git rebase` with the merge back end (Documentation/git-rebase.adoc,
//! config/rebase.adoc, config/sequencer.adoc): #154, and the interactive
//! rebase on the same state and todo list (#155).
//!
//! - Choosing the commits: `<upstream>..<branch>` without merges, oldest
//!   first, dropping commits whose change is already upstream (patch
//!   equivalence, [`crate::patch_id`]) unless `--reapply-cherry-picks`, and
//!   commits that start empty with `--no-keep-empty`; `--onto`,
//!   `--keep-base`, `--root`, `--fork-point` and the configured upstream;
//! - "up to date" and fast-forward detection, `--force-rebase`;
//! - replaying the commits like `cherry-pick` (the merge, the two-way
//!   update of the index and work tree and the conflict stages come from
//!   [`crate::sequencer`]), the original author, `--signoff`,
//!   `--committer-date-is-author-date`, `--reset-author-date`, `--empty`,
//!   signing with `-S[<keyid>]`, `--no-gpg-sign` and `commit.gpgSign`
//!   ([`crate::signing`], state file `gpg_sign_opt`);
//! - the state in `.git/rebase-merge/` in git's layout (`head-name`,
//!   `onto`, `orig-head`, `git-rebase-todo`, `done`, `msgnum`, `end`,
//!   `message`, `author-script`, `stopped-sha`, `patch`, option files),
//!   `REBASE_HEAD`, `MERGE_MSG`, `AUTO_MERGE`, `ORIG_HEAD` and the
//!   reflog messages `rebase (start|pick|continue|finish|abort)`;
//! - `--continue`, `--skip`, `--abort`, `--quit`, `--autostash`, the
//!   `pre-rebase` and `post-rewrite` hooks;
//! - the interactive machinery: the todo list ([`todo`]) with all its
//!   commands, the sequence editor (`-i`, `--edit-todo`, the help below
//!   the list, `git-rebase-todo.backup`, `rebase.abbreviateCommands`,
//!   `rebase.instructionFormat`, `rebase.missingCommitsCheck`),
//!   `--autosquash`, `--exec`, `--reschedule-failed-exec`, `reword`,
//!   `edit`, `break`, `squash`/`fixup [-C|-c]` chains with their message
//!   template, `label`/`reset`/`merge` (octopus merges and merge
//!   strategies other than `ort` through a `merge` child), `--update-refs`
//!   ([`update_refs`]) and the todo list of `--rebase-merges`
//!   ([`merges`]).
//!
//! The behaviour was derived from the documentation and differential
//! tests against the `git` binary (`tests/rebase.rs`,
//! `tests/rebase_interactive.rs`).
//!
//! [`start`], [`continue_rebase`], [`skip`], [`abort`], [`quit`] and
//! [`edit_todo`] are the entry points for `git rebase` and
//! `git pull --rebase`.

mod apply;
mod merges;
pub(crate) mod todo;
mod update_refs;

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_hook::{Input, RunOptions};
use rgit_ident::ident::Role;
use rgit_object::{Commit, Object, ObjectKind, Signature};
use rgit_refs::{RefUpdate, Transaction};

use crate::messages::rebase as msg;
use crate::messages::sequencer as seqmsg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::Mode;
use crate::repository::Repo;
use crate::sequencer::{self, Fail};
use crate::signing::{self, Sign, SignError};

/// What happens to commits that become empty (`--empty`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Empty {
    #[default]
    Drop,
    Keep,
    Stop,
}

/// The options of a new rebase.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `<upstream>` as given; `None` uses the configured upstream.
    pub upstream: Option<String>,
    /// `<branch>`: switch to it first.
    pub branch: Option<String>,
    /// `--onto <newbase>`.
    pub onto: Option<String>,
    pub keep_base: bool,
    pub root: bool,
    /// `--[no-]fork-point`; `None`: on when `<upstream>` is not given.
    pub fork_point: Option<bool>,
    /// `--force-rebase`, `--no-ff`.
    pub force: bool,
    pub quiet: bool,
    pub verbose: bool,
    /// `--[no-]stat`; `None`: `rebase.stat`.
    pub stat: Option<bool>,
    pub no_verify: bool,
    pub signoff: bool,
    pub committer_date_is_author_date: bool,
    /// `--reset-author-date`, `--ignore-date`.
    pub ignore_date: bool,
    /// `--empty`; `None`: drop.
    pub empty: Option<Empty>,
    pub no_keep_empty: bool,
    /// `--[no-]reapply-cherry-picks`; `None`: on with `--keep-base`.
    pub reapply_cherry_picks: Option<bool>,
    pub strategy: Option<String>,
    pub xopts: Vec<String>,
    /// `--[no-]autostash`; `None`: `rebase.autoStash`.
    pub autostash: Option<bool>,
    /// `-i`: let the user edit the todo list (sequence editor).
    pub interactive: bool,
    /// `--exec <cmd>`, in order.
    pub exec: Vec<String>,
    /// `--[no-]autosquash`; `None`: `rebase.autoSquash` with `-i`.
    pub autosquash: Option<bool>,
    /// `--[no-]rebase-merges[=<mode>]`; `None`: `rebase.rebaseMerges`.
    pub rebase_merges: Option<RebaseMerges>,
    /// `--[no-]reschedule-failed-exec`; `None`: `rebase.rescheduleFailedExec`.
    pub reschedule_failed_exec: Option<bool>,
    /// `-S[<keyid>]` (`Some(Some(_))`), `--no-gpg-sign` (`Some(None)`);
    /// `None`: `commit.gpgSign`.
    pub gpg_sign: Option<Option<Sign>>,
    /// `--[no-]update-refs`; `None`: `rebase.updateRefs`.
    pub update_refs: Option<bool>,
    /// The apply back end (`--apply`, `-C`, `--whitespace`).
    pub apply: bool,
    /// Options for its `am` (`-C<n>`, `--whitespace=`, `--ignore-whitespace`).
    pub am_opts: Vec<String>,
    /// `--trailer <trailer>`: added to every replayed commit.
    pub trailers: Vec<String>,
}

/// `--rebase-merges[=<mode>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseMerges {
    Off,
    NoRebaseCousins,
    RebaseCousins,
}

impl RebaseMerges {
    /// The mode named by `value` (`--rebase-merges=<mode>`,
    /// `rebase.rebaseMerges`).
    pub fn parse(value: &str) -> Option<RebaseMerges> {
        match value {
            "" | "no-rebase-cousins" => Some(RebaseMerges::NoRebaseCousins),
            "rebase-cousins" => Some(RebaseMerges::RebaseCousins),
            _ => None,
        }
    }
}

/// The state of a rebase in progress (`.git/rebase-merge`).
#[derive(Debug, Clone)]
struct State {
    /// `refs/heads/<branch>` or `detached HEAD`.
    head_name: String,
    onto: ObjectId,
    orig_head: ObjectId,
    quiet: bool,
    verbose: bool,
    signoff: bool,
    cdate_is_adate: bool,
    ignore_date: bool,
    empty: Empty,
    strategy: Option<String>,
    xopts: Vec<String>,
    autostash: Option<String>,
    squash_onto: Option<ObjectId>,
    force: bool,
    reschedule_failed_exec: bool,
    /// Sign the new commits (`gpg_sign_opt`).
    gpg_sign: Option<Sign>,
    /// `--trailer`s (state file `trailer`, one per line).
    trailers: Vec<String>,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn io_die(e: std::io::Error) -> Die {
    Die::Fatal(e.to_string())
}

fn fail_die(f: Fail) -> Die {
    match f {
        Fail::Error(m) => Die::Error(m, 1),
        Fail::Printed => Die::Exit(1),
        Fail::Die(d) => d,
    }
}

/// The absolute git directory of `repo`.
fn git_dir(repo: &Repo) -> PathBuf {
    repo.setup.cwd.join(&repo.setup.git_dir)
}

fn state_dir(repo: &Repo) -> PathBuf {
    git_dir(repo).join("rebase-merge")
}

/// The state directory of a rebase with the apply back end (started by
/// git), if one is in progress. `git am` uses the same directory without
/// the `rebasing` marker.
fn apply_state_dir(repo: &Repo) -> Option<PathBuf> {
    let dir = git_dir(repo).join("rebase-apply");
    dir.join("rebasing").exists().then_some(dir)
}

/// Whether a rebase with the merge back end is in progress.
pub fn in_progress(git_dir: &Path) -> bool {
    git_dir.join("rebase-merge").is_dir()
}

/// The reflog action: `GIT_REFLOG_ACTION` or `rebase`.
fn reflog_action() -> String {
    std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rebase".to_owned())
}

fn parse_oid(repo: &Repo, text: &str) -> Option<ObjectId> {
    ObjectId::from_hex(repo.hash, text.trim().as_bytes()).ok()
}

/// A value of the `author-script` (shell single quotes).
fn sq_quote(v: &[u8]) -> String {
    let mut out = String::from("'");
    for c in lossy(v).chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn sq_unquote(v: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => quoted = !quoted,
            '\\' if !quoted => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn author_script(author: &Signature) -> String {
    format!(
        "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE={}\n",
        sq_quote(&author.name),
        sq_quote(&author.email),
        sq_quote(format!("@{}", author.time).as_bytes()),
    )
}

/// Checks that `author-script` names the author's name, email and date
/// exactly once each and nothing else.
fn check_author_script(path: &Path) -> Result<(), String> {
    const KEYS: [&str; 3] = ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_AUTHOR_DATE"];
    let mut seen = [false; 3];
    for (k, _) in read_author_script(path) {
        let Some(i) = KEYS.iter().position(|x| *x == k) else {
            return Err(msg::unknown_author_variable(&k));
        };
        if seen[i] {
            return Err(msg::author_variable_twice(&k));
        }
        seen[i] = true;
    }
    match seen.iter().position(|s| !s) {
        Some(i) => Err(msg::missing_author_variable(KEYS[i])),
        None => Ok(()),
    }
}

fn read_author_script(path: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), sq_unquote(v)))
        .collect()
}

/// The current time in the local time zone (`--reset-author-date`).
fn now() -> rgit_object::Time {
    let (secs, tz) = rgit_ident::ident::now();
    let minutes = tz / 100 * 60 + tz % 100;
    rgit_object::Time::new(secs as i64, minutes)
}

/// The commit `name` names, if any.
fn resolve_commit(repo: &mut Repo, name: &str) -> Option<ObjectId> {
    let id = repo.resolve(name)?;
    repo.peel_to(id, ObjectKind::Commit)
}

/// The commit a todo line names, and whether it is a merge.
fn resolve_named(repo: &mut Repo, name: &str) -> Option<todo::Named> {
    let id = resolve_commit(repo, name)?;
    let merge = matches!(repo.read_object(&id), Some(Object::Commit(c))
        if c.parents().unwrap_or_default().len() > 1);
    Some(todo::Named { id, merge })
}

/// Prints the lines of a todo list that could not be parsed.
fn print_bad(ctx: &Context, repo: &Repo, bad: &[todo::BadLine]) -> Result<(), Die> {
    let advisor = ctx.advisor(&repo.config)?;
    let advise = |h: &str| {
        let _ = advisor.advise(Advice::RebaseTodoError, h);
    };
    for b in bad {
        b.print(&advise);
    }
    Ok(())
}

fn commits_of(repo: &Repo) -> rgit_merge::ObjectCommits<'_> {
    let shallow = std::fs::read_to_string(repo.setup.common_dir.join("shallow"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| ObjectId::from_hex(repo.hash, l.trim().as_bytes()).ok())
        .collect();
    let len = repo.default_abbrev();
    rgit_merge::ObjectCommits {
        objects: repo.odb(),
        shallow,
        label: Box::new(move |id| repo.abbrev(id, len).into_bytes()),
    }
}

fn merge_bases(repo: &Repo, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>, Die> {
    let commits = commits_of(repo);
    rgit_merge::merge_bases(&commits, a, b).map_err(|e| Die::Fatal(e.to_string()))
}

fn is_ancestor(repo: &Repo, a: &ObjectId, b: &ObjectId) -> Result<bool, Die> {
    Ok(a == b || merge_bases(repo, a, b)?.contains(a))
}

/// `--onto`: a commit, or `A...B` for the single merge base of `A` and
/// `B` (either defaults to `HEAD`).
fn resolve_onto(repo: &mut Repo, name: &str) -> Result<ObjectId, Die> {
    if let Some((a, b)) = name.split_once("...") {
        let a = if a.is_empty() { "HEAD" } else { a };
        let b = if b.is_empty() { "HEAD" } else { b };
        let (Some(a), Some(b)) = (resolve_commit(repo, a), resolve_commit(repo, b)) else {
            return Err(Die::Fatal(msg::invalid_onto(name)));
        };
        let bases = merge_bases(repo, &a, &b)?;
        if bases.len() != 1 {
            return Err(Die::Fatal(msg::need_one_merge_base(name)));
        }
        return Ok(bases[0]);
    }
    resolve_commit(repo, name).ok_or_else(|| Die::Fatal(msg::invalid_onto(name)))
}

/// The commits of a revision walk (`rev-list` arguments), oldest first.
fn walk(ctx: &Context, args: &[String]) -> Result<Vec<ObjectId>, Die> {
    let mut all = vec!["--do-walk".to_owned()];
    all.extend(args.iter().cloned());
    Ok(crate::history::sequencer_commits(ctx, &all, true)?.commits)
}

/// `merge-base --fork-point <ref> <head>`: the newest entry of `<ref>`'s
/// reflog (or its current value) that is an ancestor of `head`. The old
/// value of an entry counts too, so a ref whose reflog starts with an
/// update (not its creation) still knows where it was before.
pub(crate) fn fork_point(
    repo: &Repo,
    refname: &str,
    head: &ObjectId,
) -> Result<Option<ObjectId>, Die> {
    let mut candidates = Vec::new();
    if let Some(cur) = repo.refs.resolve(refname) {
        candidates.push(cur);
    }
    let text =
        rgit_refs::reflog::FilesReflog::new(&repo.setup.git_dir, &repo.setup.common_dir, repo.hash)
            .read_raw(refname)
            .ok()
            .flatten()
            .unwrap_or_default();
    let mut entries: Vec<ObjectId> = Vec::new();
    for line in text.split(|&b| b == b'\n') {
        let mut fields = line.split(|&b| b == b' ');
        for field in [fields.next(), fields.next()].into_iter().flatten() {
            if let Ok(id) = ObjectId::from_hex(repo.hash, field)
                && !id.is_null()
            {
                entries.push(id);
            }
        }
    }
    candidates.extend(entries.into_iter().rev());
    // The candidate closest to `head` among those it contains.
    let mut seen = HashSet::new();
    let mut best: Option<ObjectId> = None;
    for c in candidates {
        if !seen.insert(c) {
            continue;
        }
        if !matches!(repo.read_object(&c), Some(Object::Commit(_))) || !is_ancestor(repo, &c, head)?
        {
            continue;
        }
        match best {
            Some(b) if !is_ancestor(repo, &b, &c)? => {}
            _ => best = Some(c),
        }
    }
    Ok(best)
}

/// Whether the index or the work tree differ from `HEAD`: (unstaged,
/// uncommitted).
fn local_changes(repo: &mut Repo) -> Result<(bool, bool), Die> {
    let head = sequencer::head_commit(repo);
    let (index, mtime) = repo.read_index()?;
    let staged = match head {
        Some(h) => {
            let tree = sequencer::tree_of(repo, &h).map_err(fail_die)?;
            !sequencer::index_matches_tree(repo, &index, &tree)?
        }
        None => !index.entries().is_empty(),
    };
    let checker = repo.checker(mtime)?;
    let mut unstaged = false;
    for e in index.entries() {
        if e.stage().is_conflict() {
            unstaged = true;
            break;
        }
        if e.mode.is_gitlink() {
            continue;
        }
        match checker.check(e) {
            Ok(rgit_worktree::stat::Status::Unchanged | rgit_worktree::stat::Status::StatOnly) => {}
            _ => {
                unstaged = true;
                break;
            }
        }
    }
    Ok((unstaged, staged))
}

// ----------------------------------------------------------------------
// Reference updates.

fn ref_tx(ctx: &Context, repo: &Repo, update: RefUpdate, symbolic_head: bool) -> Result<(), Die> {
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects);
    if symbolic_head {
        hooks.mark_symbolic("HEAD");
    }
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// Detaches `HEAD` at `new`.
fn detach_head(ctx: &Context, repo: &Repo, new: ObjectId, message: &str) -> Result<(), Die> {
    ref_tx(
        ctx,
        repo,
        RefUpdate::update("HEAD", new, None)
            .no_deref(true)
            .message(message.to_owned()),
        false,
    )
}

/// Points `HEAD` at the branch `full`.
fn attach_head(ctx: &Context, repo: &Repo, full: &str, message: &str) -> Result<(), Die> {
    ref_tx(
        ctx,
        repo,
        RefUpdate::symref_update("HEAD", full, None)
            .no_deref(true)
            .message(message.to_owned()),
        true,
    )
}

fn write_orig_head(ctx: &Context, repo: &Repo, id: ObjectId) -> Result<(), Die> {
    ref_tx(
        ctx,
        repo,
        RefUpdate::update("ORIG_HEAD", id, None)
            .no_deref(true)
            .message(crate::messages::reset::UPDATING_ORIG_HEAD),
        false,
    )
}

// ----------------------------------------------------------------------
// Autostash.

/// Runs `rgit <args>`; (exit code, stdout).
fn run_child(ctx: &Context, args: &[&str], capture: bool) -> Result<(i32, Vec<u8>), Die> {
    let _ = std::io::stdout().flush();
    let exe = std::env::current_exe().map_err(io_die)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).envs(ctx.child_env());
    if capture {
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let out = cmd.output().map_err(io_die)?;
        Ok((out.status.code().unwrap_or(128), out.stdout))
    } else {
        let status = cmd.status().map_err(io_die)?;
        Ok((status.code().unwrap_or(128), Vec::new()))
    }
}

/// Stashes the local changes, if there are any, as [`create_autostash`]
/// does (`merge --autostash`).
pub(crate) fn autostash_local_changes(ctx: &Context) -> Result<Option<String>, Die> {
    let (unstaged, staged) = local_changes(&mut Repo::open(ctx, Mode::Required)?)?;
    if unstaged || staged {
        create_autostash(ctx)
    } else {
        Ok(None)
    }
}

/// `--autostash`: stashes the local changes and resets to `HEAD`;
/// returns the stash commit.
fn create_autostash(ctx: &Context) -> Result<Option<String>, Die> {
    let (code, out) = run_child(ctx, &["stash", "create", msg::AUTOSTASH_MESSAGE], true)?;
    let hex = lossy(&out).trim().to_owned();
    if code != 0 || hex.is_empty() {
        return Err(Die::Fatal(seqmsg::could_not_read("stash")));
    }
    let mut repo = Repo::open(ctx, Mode::Required)?;
    if let Some(id) = parse_oid(&repo, &hex) {
        println!(
            "{}",
            msg::created_autostash(&repo.abbrev(&id, repo.default_abbrev()))
        );
    }
    if let Some(h) = sequencer::head_commit(&mut repo) {
        let tree = sequencer::tree_of(&repo, &h).map_err(fail_die)?;
        crate::switch_branch::move_index(&repo, tree, tree, true)?;
    }
    Ok(Some(hex))
}

/// Applies the autostash `hex`; on conflicts it is kept in the stash list.
pub(crate) fn apply_autostash(ctx: &Context, hex: &str) -> Result<(), Die> {
    let (code, _) = run_child(ctx, &["stash", "apply", hex], true)?;
    if code == 0 {
        eprintln!("{}", msg::APPLIED_AUTOSTASH);
        return Ok(());
    }
    run_child(
        ctx,
        &["stash", "store", "-q", "-m", msg::AUTOSTASH_MESSAGE, hex],
        true,
    )?;
    // Conflicts left in the index and work tree get their own message.
    let conflicted = Repo::open(ctx, Mode::Required)
        .and_then(|r| r.read_index())
        .is_ok_and(|(index, _)| index.has_conflicts());
    if conflicted {
        eprintln!("{}", msg::AUTOSTASH_APPLIED_WITH_CONFLICTS);
    } else {
        eprint!("{}", msg::AUTOSTASH_CONFLICTS);
    }
    Ok(())
}

// ----------------------------------------------------------------------
// Starting a rebase.

/// The upstream configured for the current branch; prints git's
/// explanation and fails when there is none.
fn configured_upstream(repo: &Repo) -> Result<String, Die> {
    let Some(branch) = repo
        .refs
        .head_target()
        .and_then(|b| b.strip_prefix("refs/heads/").map(str::to_owned))
    else {
        print!("{}", msg::NOT_ON_BRANCH);
        println!();
        return Err(Die::Exit(1));
    };
    let config = &repo.config;
    let up = rgit_remote::BranchConfig::from_config(config, branch.as_bytes())
        .ok()
        .and_then(|b| {
            let remotes = rgit_remote::Remotes::from_config(config).ok()?;
            rgit_remote::upstream_ref(&remotes, Some(&repo.setup.git_dir), Some(&b)).ok()
        });
    match up {
        Some(r) => Ok(r.to_string()),
        None => {
            print!("{}", msg::no_tracking(&branch));
            println!();
            Err(Die::Exit(1))
        }
    }
}

/// Starts a rebase.
pub fn start(ctx: &mut Context, o: &Options) -> CmdResult {
    let mut repo = Repo::open(ctx, Mode::Required)?;
    repo.work_tree()?;
    crate::commands::commit::warn_comment_char_auto(&repo);
    for name in ["rebase-merge", "rebase-apply"] {
        if git_dir(&repo).join(name).is_dir() {
            let shown = repo.setup.git_dir.join(name);
            return Err(Die::Fatal(msg::already_in_progress(
                name,
                &shown.to_string_lossy(),
            )));
        }
    }
    let config_bool = |repo: &Repo, key: &str| repo.config.bool(key).ok().flatten();
    let quiet = o.quiet;
    let stat = !quiet
        && (o.verbose
            || o.stat
                .unwrap_or_else(|| config_bool(&repo, "rebase.stat").unwrap_or(false)));
    let autostash = o
        .autostash
        .unwrap_or_else(|| config_bool(&repo, "rebase.autostash").unwrap_or(false));

    // The upstream.
    let (upstream_name, upstream) = if o.root {
        (None, None)
    } else {
        let name = match &o.upstream {
            Some(u) => u.clone(),
            None => configured_upstream(&repo)?,
        };
        let shown = if name == "-" {
            "@{-1}".to_owned()
        } else {
            name.clone()
        };
        let id = resolve_commit(&mut repo, &shown)
            .ok_or_else(|| Die::Fatal(msg::invalid_upstream(&name)))?;
        (Some(name), Some(id))
    };

    // The branch to rebase.
    let (head_name, orig_head, switch_to) = match &o.branch {
        Some(b) => {
            let full = format!("refs/heads/{b}");
            match repo.refs.resolve(&full) {
                Some(id) => {
                    if let Some(path) =
                        crate::worktrees::checked_out_elsewhere(&repo.setup, &repo.config, &full)
                    {
                        return Err(Die::Fatal(crate::messages::worktree::already_used_by(
                            b,
                            &path.display().to_string(),
                        )));
                    }
                    (full, id, Some(b.clone()))
                }
                None => {
                    let id = resolve_commit(&mut repo, b)
                        .ok_or_else(|| Die::Fatal(msg::no_such_branch(b)))?;
                    (msg::DETACHED_HEAD.to_owned(), id, Some(b.clone()))
                }
            }
        }
        None => {
            let head = sequencer::head_commit(&mut repo)
                .ok_or_else(|| Die::Fatal(msg::invalid_upstream("HEAD")))?;
            let name = repo
                .refs
                .head_target()
                .unwrap_or_else(|| msg::DETACHED_HEAD.to_owned());
            (name, head, None)
        }
    };

    // The new base.
    let mut squash_onto = None;
    let (onto, onto_name) = if let Some(on) = &o.onto {
        (resolve_onto(&mut repo, on)?, on.clone())
    } else if o.keep_base {
        let up = upstream.expect("upstream with --keep-base");
        let bases = merge_bases(&repo, &up, &orig_head)?;
        let branch = o.branch.clone().unwrap_or_else(|| {
            head_name
                .strip_prefix("refs/heads/")
                .unwrap_or("HEAD")
                .to_owned()
        });
        let name = format!("{}...{branch}", upstream_name.clone().unwrap_or_default());
        if bases.len() != 1 {
            return Err(Die::Fatal(msg::need_one_merge_base_with_branch(&name)));
        }
        (bases[0], name)
    } else if o.root {
        let id = squash_onto_commit(&repo)?;
        squash_onto = Some(id);
        (id, id.to_hex())
    } else {
        let up = upstream.expect("upstream");
        (up, upstream_name.clone().unwrap_or_default())
    };

    // The commits after the fork point.
    // With --root and --onto, the commits already in the new base are
    // left out.
    let root_onto = (o.root && o.onto.is_some()).then_some(onto);
    let mut restrict = upstream.or(root_onto);
    let fork = o
        .fork_point
        .unwrap_or(o.upstream.is_none() && !o.keep_base && !o.root)
        && o.fork_point != Some(false)
        && !(o.fork_point.is_none() && config_bool(&repo, "rebase.forkpoint") == Some(false));
    if fork && let (Some(name), Some(_)) = (&upstream_name, upstream) {
        let full = full_ref_name(&repo, name);
        if let Some(fp) = fork_point(&repo, &full, &orig_head)? {
            restrict = Some(fp);
        }
    }

    // Local changes.
    let mut stash = None;
    {
        let (unstaged, staged) = local_changes(&mut repo)?;
        if unstaged || staged {
            if autostash {
                drop(repo);
                stash = create_autostash(ctx)?;
                repo = Repo::open(ctx, Mode::Required)?;
            } else {
                eprintln!(
                    "error: {}",
                    if unstaged {
                        msg::UNSTAGED_CHANGES
                    } else {
                        msg::UNCOMMITTED_CHANGES
                    }
                );
                eprintln!("error: {}", msg::COMMIT_OR_STASH);
                return Ok(1);
            }
        }
    }

    // The interactive machinery.
    let autosquash = o.autosquash.unwrap_or_else(|| {
        o.interactive && config_bool(&repo, "rebase.autosquash").unwrap_or(false)
    });
    let configured = match repo.config.string("rebase.rebasemerges").ok().flatten() {
        Some(v) => {
            let v = v.to_string();
            match v.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => RebaseMerges::NoRebaseCousins,
                "false" | "no" | "off" | "0" => RebaseMerges::Off,
                _ => RebaseMerges::parse(&v)
                    .ok_or_else(|| Die::Fatal(msg::invalid_rebase_merges(&v)))?,
            }
        }
        None => RebaseMerges::Off,
    };
    let rebase_merges = o.rebase_merges.unwrap_or(configured);
    let rebase_merges = (rebase_merges != RebaseMerges::Off).then_some(rebase_merges);
    let skip_up_to_date = o.interactive || !o.exec.is_empty() || o.autosquash == Some(true);

    // Up to date?
    let branch_short = head_name.strip_prefix("refs/heads/").map(str::to_owned);
    if let Some(r) = restrict
        && !o.root
        && !skip_up_to_date
    {
        let bases = merge_bases(&repo, &r, &orig_head)?;
        let linear = bases.len() == 1
            && bases[0] == onto
            && walk(
                ctx,
                &[
                    "--merges".to_owned(),
                    format!("^{}", r.to_hex()),
                    orig_head.to_hex(),
                ],
            )?
            .is_empty();
        if linear {
            if !(o.force
                || o.signoff
                || o.committer_date_is_author_date
                || o.ignore_date
                || !o.trailers.is_empty())
            {
                if let Some(b) = &switch_to {
                    let current = sequencer::head_commit(&mut repo);
                    let old_tree = match current {
                        Some(c) => sequencer::tree_of(&repo, &c).map_err(fail_die)?,
                        None => ObjectId::empty_tree(repo.hash),
                    };
                    let new_tree = sequencer::tree_of(&repo, &orig_head).map_err(fail_die)?;
                    let refname = head_name.starts_with("refs/").then(|| head_name.clone());
                    crate::convert::set_checkout_meta(refname, Some(&orig_head));
                    let moved = crate::switch_branch::move_index(&repo, old_tree, new_tree, false);
                    crate::convert::set_checkout_meta(None, None);
                    if let Err(e) = moved {
                        // The branch cannot be checked out: the autostash
                        // comes back.
                        if !matches!(e, Die::Exit(_)) {
                            return Err(e);
                        }
                        eprintln!("error: {}", msg::could_not_switch(b));
                        drop(repo);
                        if let Some(s) = stash {
                            apply_autostash(ctx, &s)?;
                        }
                        return Ok(1);
                    }
                    let what = format!("{}: {}", reflog_action(), msg::checkout(b));
                    if head_name.starts_with("refs/") {
                        attach_head(ctx, &repo, &head_name, &what)?;
                    } else {
                        detach_head(ctx, &repo, orig_head, &what)?;
                    }
                    crate::hooks::post_checkout(&repo, current, Some(orig_head), true)?;
                }
                crate::maintenance::run_auto(ctx);
                if !quiet {
                    println!("{}", msg::up_to_date(branch_short.as_deref(), false));
                }
                if let Some(s) = stash {
                    apply_autostash(ctx, &s)?;
                }
                return Ok(0);
            }
            if !quiet {
                println!("{}", msg::up_to_date(branch_short.as_deref(), true));
            }
        }
    }

    // The pre-rebase hook.
    if !o.no_verify {
        let hooks = crate::hooks::for_repo(ctx, &repo.setup, &repo.config)?;
        if hooks.exists("pre-rebase") {
            let mut args: Vec<String> = Vec::new();
            if let Some(u) = &o.upstream {
                args.push(u.clone());
            } else if o.root {
                args.push("--root".to_owned());
            } else if let Some(u) = &upstream_name {
                args.push(u.clone());
            }
            if let Some(b) = &o.branch {
                args.push(b.clone());
            }
            let outcome = crate::hooks::run(&hooks, "pre-rebase", &RunOptions::with_args(args))?;
            if !outcome.ok() {
                if let Some(s) = stash {
                    apply_autostash(ctx, &s)?;
                }
                return Err(Die::Error(msg::HOOK_REFUSED.to_owned(), 1));
            }
        }
    }

    // The diffstat of what changed upstream.
    if stat {
        let bases = merge_bases(&repo, &onto, &orig_head)?;
        let n = onto.to_hex();
        let b = match bases.first() {
            Some(base) => {
                if o.verbose {
                    println!("{}", msg::changes_from(&base.to_hex(), &n));
                }
                base.to_hex()
            }
            // Unrelated histories: everything up to the new base.
            None => {
                if o.verbose {
                    println!("{}", msg::changes_to(&n));
                }
                ObjectId::empty_tree(repo.hash).to_hex()
            }
        };
        run_child(ctx, &["diff", "--stat", "--summary", &b, &n], false)?;
    }

    // The apply back end.
    if o.apply {
        let state = State {
            head_name,
            onto,
            orig_head,
            quiet,
            verbose: o.verbose,
            signoff: o.signoff,
            cdate_is_adate: o.committer_date_is_author_date,
            ignore_date: o.ignore_date,
            empty: Empty::Drop,
            strategy: None,
            xopts: Vec::new(),
            autostash: stash,
            squash_onto: None,
            force: o.force,
            reschedule_failed_exec: false,
            gpg_sign: signing::resolve(&repo.config, o.gpg_sign.clone()),
            trailers: Vec::new(),
        };
        if !quiet {
            println!("{}", msg::REWINDING);
        }
        let action = reflog_action();
        let fast_forward = is_ancestor(&repo, &orig_head, &onto)?;
        if let Some(code) = checkout_onto(
            ctx,
            &mut repo,
            onto,
            orig_head,
            (&action, &onto_name),
            state.autostash.as_deref(),
        )? {
            return Ok(code);
        }
        drop(repo);
        if fast_forward {
            // Nothing to replay: the branch moves to the new base.
            let branch = branch_short.as_deref().unwrap_or("HEAD");
            println!("{}", msg::fast_forwarded(branch, &onto_name));
            return apply::finish(ctx, &state);
        }
        return apply::start(
            ctx,
            &apply::Start {
                restrict,
                state: &state,
                am_opts: &o.am_opts,
            },
        );
    }

    // The todo list.
    let reapply = o.reapply_cherry_picks.unwrap_or(o.keep_base);
    let comment = crate::commands::commit::comment_string(&repo, b"")?;
    let mut todo = match rebase_merges {
        Some(mode) => merges::todo_list(
            ctx,
            &repo,
            &merges::Params {
                restrict,
                upstream: upstream.or(root_onto),
                head: orig_head,
                reapply,
                keep_empty: !o.no_keep_empty,
                quiet,
                cousins: mode == RebaseMerges::RebaseCousins,
                root_onto: root_onto.is_some(),
            },
        )?,
        None => todo::Todo::from_items(todo_list(
            ctx,
            &repo,
            restrict,
            upstream.or(root_onto),
            orig_head,
            reapply,
            !o.no_keep_empty,
            quiet,
        )?),
    };
    let update_refs = o
        .update_refs
        .unwrap_or_else(|| config_bool(&repo, "rebase.updaterefs").unwrap_or(false));
    if update_refs {
        update_refs::add_commands(&repo, &mut todo, &head_name, &comment);
    }
    if let Err(e) = instruction_format(ctx, &repo, &mut todo) {
        if let Some(s) = &stash {
            drop(repo);
            apply_autostash(ctx, s)?;
        }
        return Err(e);
    }
    if autosquash {
        todo::autosquash(&mut repo, &mut todo);
    }
    todo::add_exec(&mut todo, &o.exec);
    if o.interactive && todo.count() == 0 {
        todo.lines
            .push(todo::Line::Cmd(todo::Item::new(todo::Cmd::Noop, "")));
    }

    let empty = o.empty.unwrap_or(if o.interactive {
        Empty::Stop
    } else if !o.exec.is_empty() {
        Empty::Keep
    } else {
        Empty::Drop
    });
    let reschedule_failed_exec = o
        .reschedule_failed_exec
        .unwrap_or_else(|| config_bool(&repo, "rebase.reschedulefailedexec").unwrap_or(false));
    let state = State {
        head_name,
        onto,
        orig_head,
        quiet,
        verbose: o.verbose,
        signoff: o.signoff,
        cdate_is_adate: o.committer_date_is_author_date,
        ignore_date: o.ignore_date,
        empty,
        strategy: o.strategy.clone(),
        xopts: o.xopts.clone(),
        autostash: stash,
        squash_onto,
        force: o.force
            || o.signoff
            || o.committer_date_is_author_date
            || o.ignore_date
            || !o.trailers.is_empty(),
        reschedule_failed_exec,
        gpg_sign: signing::resolve(&repo.config, o.gpg_sign.clone()),
        trailers: o.trailers.clone(),
    };
    if o.interactive {
        // git reads the setting once more before writing the list.
        let _ = missing_commits_check(&repo);
    }
    let help = {
        let len = repo.default_abbrev();
        let from = if o.keep_base { Some(onto) } else { upstream };
        let range = match from {
            Some(f) => format!("{}..{}", repo.abbrev(&f, len), repo.abbrev(&orig_head, len)),
            None => repo.abbrev(&orig_head, len),
        };
        let short_onto = repo.abbrev(&onto, len);
        todo_help(
            &repo,
            &comment,
            Some(&msg::todo_header(&range, &short_onto, todo.count())),
        )
    };
    write_state(&repo, &state, &todo, &help, o.interactive)?;
    if !o.interactive {
        // The comments of the generated list stay in the backup only.
        todo.strip_comments(&comment);
        write(&state_dir(&repo).join("git-rebase-todo"), todo.text())?;
    }
    let action = reflog_action();
    let onto_name = onto_name.clone();

    // Let the user edit the list.
    if o.interactive {
        let path = state_dir(&repo).join("git-rebase-todo");
        let shown = render_for_edit(&repo, &todo) + &help;
        write(&path, shown)?;
        if let Err(code) = sequence_edit(ctx, &repo, &path) {
            // The rebase does not start.
            let _ = std::fs::remove_dir_all(state_dir(&repo));
            drop(repo);
            if let Some(s) = &state.autostash {
                apply_autostash(ctx, s)?;
            }
            return Ok(code);
        }
        let text = std::fs::read_to_string(&path).map_err(io_die)?;
        let has_commands = text.lines().any(|l| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with(comment.as_str())
        });
        if !has_commands {
            eprintln!("error: {}", msg::NOTHING_TO_DO);
            let _ = std::fs::remove_dir_all(state_dir(&repo));
            drop(repo);
            if let Some(s) = &state.autostash {
                apply_autostash(ctx, s)?;
            }
            return Ok(1);
        }
        match validate(ctx, &mut repo, &text, &comment, None) {
            Ok(t) => todo = t,
            Err(()) => {
                let autostash = state.autostash.as_deref();
                checkout_onto(
                    ctx,
                    &mut repo,
                    onto,
                    orig_head,
                    (&action, &onto_name),
                    autostash,
                )?;
                return Ok(1);
            }
        }
        write(&path, todo.text())?;
    }
    update_refs::sync(&repo, &todo)?;

    // Commits that need not be replayed: the new base moves on to them.
    let mut start_at = onto;
    let mut skipped = Vec::new();
    if !state.force {
        while let Some(first) = todo.peek() {
            if first.cmd == todo::Cmd::Drop {
                skipped.push(todo.pop().expect("peeked"));
                continue;
            }
            if first.cmd != todo::Cmd::Pick {
                break;
            }
            let Some(c) = first.commit else {
                break;
            };
            if !matches!(repo.read_object(&c), Some(Object::Commit(cm))
                if cm.parents().unwrap_or_default() == [start_at])
            {
                break;
            }
            skipped.push(todo.pop().expect("peeked"));
            start_at = c;
        }
    }
    if !skipped.is_empty() {
        let dir = state_dir(&repo);
        let mut done = String::new();
        for i in &skipped {
            done.push_str(&i.line());
            done.push('\n');
        }
        write(&dir.join("done"), done)?;
        write(&dir.join("git-rebase-todo"), todo.text())?;
    }

    // Check out the new base.
    let autostash = state.autostash.as_deref();
    if let Some(code) = checkout_onto(
        ctx,
        &mut repo,
        start_at,
        orig_head,
        (&action, &onto_name),
        autostash,
    )? {
        return Ok(code);
    }
    drop(repo);
    let mut rb = Rebase {
        ctx,
        state,
        action,
        apply: false,
    };
    if let (Some(last), Some(next)) = (skipped.last(), todo.peek())
        && last.cmd == todo::Cmd::Pick
        && next.cmd.melds()
        && let Some(c) = last.commit
    {
        rb.record_rewritten(&c, &c, Some(next))?;
    }
    // A list of nothing but `noop` has nothing to run.
    if todo.items().all(|i| i.cmd == todo::Cmd::Noop) {
        return rb.finish();
    }
    rb.run(todo)
}

/// Detaches `HEAD` at `onto` to start replaying; `Some(code)` when the
/// work tree could not be updated (the rebase is cancelled).
fn checkout_onto(
    ctx: &mut Context,
    repo: &mut Repo,
    onto: ObjectId,
    orig_head: ObjectId,
    (action, onto_name): (&str, &str),
    autostash: Option<&str>,
) -> Result<Option<i32>, Die> {
    let current = sequencer::head_commit(repo);
    let old_tree = match current {
        Some(c) => sequencer::tree_of(repo, &c).map_err(fail_die)?,
        None => ObjectId::empty_tree(repo.hash),
    };
    let onto_tree = sequencer::tree_of(repo, &onto).map_err(fail_die)?;
    if let Err(e) = crate::switch_branch::move_index(repo, old_tree, onto_tree, false) {
        let _ = std::fs::remove_dir_all(state_dir(repo));
        if let Some(s) = autostash {
            apply_autostash(ctx, s)?;
        }
        if let Die::Exit(code) = e {
            eprintln!("error: {}", msg::COULD_NOT_DETACH);
            return Ok(Some(code));
        }
        return Err(e);
    }
    // ORIG_HEAD only once the checkout worked (git leaves it alone when
    // local changes are in the way).
    write_orig_head(ctx, repo, orig_head)?;
    detach_head(
        ctx,
        repo,
        onto,
        &msg::reflog(action, "start", &msg::checkout(onto_name)),
    )?;
    crate::hooks::post_checkout(repo, Some(orig_head), Some(onto), true)?;
    Ok(None)
}

/// `rebase.instructionFormat`: the description after each picked
/// commit.
fn instruction_format(ctx: &Context, repo: &Repo, todo: &mut todo::Todo) -> Result<(), Die> {
    let ids: Vec<ObjectId> = todo
        .items()
        .filter(|i| i.cmd.picks())
        .filter_map(|i| i.commit)
        .collect();
    let Some(by_id) = formatted_onelines(ctx, repo, &ids)? else {
        return Ok(());
    };
    let empty_mark = format!(
        " {} empty",
        crate::commands::commit::comment_string(repo, b"")?
    );
    for l in &mut todo.lines {
        if let todo::Line::Cmd(i) = l
            && i.cmd.picks()
            && let Some(c) = i.commit
            && let Some(d) = by_id.get(&c)
        {
            let empty = i.arg.ends_with(&empty_mark);
            i.arg = format!("# {d}");
            if empty {
                i.arg.push_str(&empty_mark);
            }
        }
    }
    Ok(())
}

/// The descriptions of `ids` in `rebase.instructionFormat`; `None` when
/// it is not set.
fn formatted_onelines(
    ctx: &Context,
    repo: &Repo,
    ids: &[ObjectId],
) -> Result<Option<std::collections::HashMap<ObjectId, String>>, Die> {
    let Some(fmt) = repo
        .config
        .string("rebase.instructionformat")
        .ok()
        .flatten()
        .map(|f| f.to_string())
        .filter(|f| !f.is_empty())
    else {
        return Ok(None);
    };
    // git formats `# <format>`: without a placeholder that is taken for
    // the name of a pretty format, which does not exist.
    if !fmt.contains('%') {
        return Err(Die::Fatal(crate::messages::rev_list::invalid_pretty(
            &format!("# {fmt}"),
        )));
    }
    let mut by_id = std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(Some(by_id));
    }
    let mut args = vec![
        "log".to_owned(),
        "--no-walk=unsorted".to_owned(),
        "--no-color".to_owned(),
        "-z".to_owned(),
        // `%h` shows the full id, as in git.
        "--no-abbrev".to_owned(),
        format!("--format=%H {fmt}"),
    ];
    args.extend(ids.iter().map(ObjectId::to_hex));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (_, out) = run_child(ctx, &refs, true)?;
    for entry in out.split(|&b| b == 0) {
        let text = lossy(entry);
        if let Some((id, rest)) = text.split_once(' ')
            && let Some(id) = parse_oid(repo, id)
        {
            let line = rest.lines().next().unwrap_or_default().to_owned();
            by_id.insert(id, line);
        }
    }
    Ok(Some(by_id))
}

/// The help appended to the todo list the user edits; `header` is the
/// "Rebase a..b onto c" line of a new rebase (`None` for `--edit-todo`).
fn todo_help(repo: &Repo, comment: &str, header: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(h) = header {
        text.push('\n');
        text.push_str(h);
        text.push('\n');
    }
    text.push_str(msg::TODO_HELP);
    if missing_commits_check(repo) != Check::Error {
        text.push_str(msg::TODO_HELP_LOST);
    } else {
        text.push_str(msg::TODO_HELP_DROP);
    }
    text.push_str(if header.is_some() {
        msg::TODO_HELP_ABORT
    } else {
        msg::TODO_HELP_EDIT_TODO
    });
    let mut out = String::new();
    let mut first = true;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if first && header.is_some() && body.is_empty() {
            // The blank line between the list and the help.
            out.push('\n');
        } else if body.is_empty() {
            out.push_str(comment);
            out.push('\n');
        } else {
            out.push_str(comment);
            out.push(' ');
            out.push_str(body);
            out.push('\n');
        }
        first = false;
    }
    out
}

/// The todo list as the user edits it: abbreviated object names, and
/// one-letter commands with `rebase.abbreviateCommands`.
fn render_for_edit(repo: &Repo, todo: &todo::Todo) -> String {
    let abbrev_cmd = repo
        .config
        .bool("rebase.abbreviatecommands")
        .ok()
        .flatten()
        .unwrap_or(false);
    let len = repo.default_abbrev();
    let mut t = todo.clone();
    // Leading blank lines are not shown.
    while matches!(t.lines.first(), Some(todo::Line::Comment(c)) if c.trim().is_empty()) {
        t.lines.remove(0);
    }
    t.render(abbrev_cmd, &|id: &ObjectId| repo.abbrev(id, len))
}

/// Runs the sequence editor on `path`; `Err(code)` when it failed.
fn sequence_edit(ctx: &Context, repo: &Repo, path: &Path) -> Result<(), i32> {
    let env = rgit_config::SystemEnv::default();
    let editor = match rgit_ident::editor::sequence_editor(&repo.config, &env) {
        Ok(e) => e,
        Err(p) => {
            p.print();
            return Err(rgit_options::EXIT_FATAL);
        }
    };
    let hint = ctx
        .advisor(&repo.config)
        .ok()
        .and_then(|a| a.enabled(Advice::WaitingForEditor).ok())
        .unwrap_or(true);
    let opts = rgit_ident::editor::LaunchOptions {
        hint,
        env: ctx.child_env(),
        cwd: None,
    };
    let _ = std::io::stdout().flush();
    if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), path, &opts) {
        e.print();
        return Err(1);
    }
    Ok(())
}

/// `rebase.missingCommitsCheck`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Ignore,
    Warn,
    Error,
}

/// `rebase.missingCommitsCheck`; an unknown value is ignored with a
/// warning.
fn missing_commits_check(repo: &Repo) -> Check {
    let Some(value) = repo
        .config
        .string("rebase.missingcommitscheck")
        .ok()
        .flatten()
        .map(|v| v.to_string())
    else {
        return Check::Ignore;
    };
    match value.to_ascii_lowercase().as_str() {
        "warn" => Check::Warn,
        "error" => Check::Error,
        "ignore" => Check::Ignore,
        _ => {
            eprintln!("warning: {}", msg::unrecognized_check_level(&value));
            Check::Ignore
        }
    }
}

/// Parses the edited todo list and checks it: unparsable lines and, with
/// `rebase.missingCommitsCheck`, commits that disappeared from the list
/// (compared with `git-rebase-todo.backup`, plus `done` lines given in
/// `done`). Problems are printed; `Err` when the rebase cannot go on.
fn validate(
    ctx: &Context,
    repo: &mut Repo,
    text: &str,
    comment: &str,
    extra: Option<&str>,
) -> Result<todo::Todo, ()> {
    let mut resolve = |name: &str| resolve_named(repo, name);
    let mut todo = match todo::parse(text, comment, &mut resolve) {
        Ok(t) => t,
        Err(bad) => {
            let _ = print_bad(ctx, repo, &bad);
            match extra {
                Some(e) => eprintln!("error: {e}"),
                None => eprintln!("{}", msg::FIX_WITH_EDIT_TODO),
            }
            return Err(());
        }
    };
    todo.strip_comments(comment);
    let dir = state_dir(repo);
    let done_text = std::fs::read_to_string(dir.join("done")).unwrap_or_default();
    if done_text.trim().is_empty()
        && let Some(first) = todo.peek()
        && first.cmd.melds()
    {
        eprintln!("error: {}", msg::meld_first(first.cmd.name()));
        match extra {
            Some(e) => eprintln!("error: {e}"),
            None => eprintln!("{}", msg::FIX_WITH_EDIT_TODO),
        }
        return Err(());
    }
    let check = missing_commits_check(repo);
    if check == Check::Ignore {
        return Ok(todo);
    }
    let backup = std::fs::read_to_string(dir.join("git-rebase-todo.backup")).unwrap_or_default();
    let done = done_text;
    let mut resolve = |name: &str| resolve_named(repo, name);
    let original = todo::parse(&backup, comment, &mut resolve).unwrap_or_default();
    let done = todo::parse(&done, comment, &mut resolve).unwrap_or_default();
    let present: HashSet<ObjectId> = todo::commits(&todo)
        .into_iter()
        .chain(todo::commits(&done))
        .collect();
    let missing: Vec<&todo::Item> = original
        .items()
        .filter(|i| i.cmd.picks() || i.cmd == todo::Cmd::Drop)
        .filter(|i| i.commit.is_some_and(|c| !present.contains(&c)))
        .collect();
    if missing.is_empty() {
        let _ = std::fs::remove_file(dir.join("dropped"));
        return Ok(todo);
    }
    let len = repo.default_abbrev();
    let mut out = format!("{}\n", msg::DROPPED_WARNING);
    for i in missing.iter().rev() {
        let c = i.commit.expect("commit");
        out.push_str(&format!(" - {} {}\n", repo.abbrev(&c, len), i.arg));
    }
    out.push_str(msg::DROPPED_HINT);
    out.push('\n');
    out.push_str(msg::FIX_WITH_EDIT_TODO);
    eprintln!("{out}");
    if check == Check::Error {
        let mut list = String::new();
        for i in &missing {
            list.push_str(&i.commit.expect("commit").to_hex());
            list.push('\n');
        }
        let _ = std::fs::write(dir.join("dropped"), list);
        return Err(());
    }
    Ok(todo)
}

/// The full name of the ref `name` (for its reflog).
fn full_ref_name(repo: &Repo, name: &str) -> String {
    if name.starts_with("refs/") {
        return name.to_owned();
    }
    for prefix in ["refs/", "refs/tags/", "refs/heads/", "refs/remotes/"] {
        let full = format!("{prefix}{name}");
        if repo.refs.resolve(&full).is_some() {
            return full;
        }
    }
    let full = format!("refs/remotes/{name}/HEAD");
    if repo.refs.resolve(&full).is_some() {
        return full;
    }
    name.to_owned()
}

/// `--root` without `--onto`: an empty root commit to replay onto.
fn squash_onto_commit(repo: &Repo) -> Result<ObjectId, Die> {
    let empty = ObjectId::empty_tree(repo.hash);
    let author = sequencer::ident(repo, Role::Author)?;
    let committer = sequencer::ident(repo, Role::Committer)?;
    let new = rgit_commit::NewCommit {
        tree: empty,
        parents: &[],
        author: &author,
        committer: &committer,
        encoding: None,
        message: b"",
    };
    rgit_commit::create_commit(repo.odb(), &new).map_err(|e| Die::Fatal(e.to_string()))
}

/// The commits to replay, oldest first.
#[allow(clippy::too_many_arguments)]
fn todo_list(
    ctx: &Context,
    repo: &Repo,
    restrict: Option<ObjectId>,
    upstream: Option<ObjectId>,
    head: ObjectId,
    reapply: bool,
    keep_empty: bool,
    quiet: bool,
) -> Result<Vec<todo::Item>, Die> {
    let mut args = vec!["--topo-order".to_owned(), "--no-merges".to_owned()];
    if let Some(r) = restrict {
        args.push(format!("^{}", r.to_hex()));
    }
    args.push(head.to_hex());
    let commits = walk(ctx, &args)?;
    let skipped = if reapply {
        HashSet::new()
    } else {
        upstream_equivalents(ctx, repo, &commits, upstream, head)?
    };

    let mut items = Vec::new();
    let mut warned = false;
    for c in commits {
        if skipped.contains(&c) {
            warn_skipped(repo, &c, quiet);
            warned = true;
            continue;
        }
        let Some(Object::Commit(commit)) = repo.read_object(&c) else {
            continue;
        };
        if !keep_empty && is_empty_commit(repo, &commit) {
            continue;
        }
        items.push(todo::Item::pick(c, pick_description(repo, &commit)?));
    }
    if warned {
        advise_skipped(ctx, repo)?;
    }
    Ok(items)
}

/// The commits of `commits` (not merges) whose change is already in
/// `upstream` (patch equivalence).
fn upstream_equivalents(
    ctx: &Context,
    repo: &Repo,
    commits: &[ObjectId],
    upstream: Option<ObjectId>,
    head: ObjectId,
) -> Result<HashSet<ObjectId>, Die> {
    let mut skipped: HashSet<ObjectId> = HashSet::new();
    let Some(up) = upstream else {
        return Ok(skipped);
    };
    if commits.is_empty() {
        return Ok(skipped);
    }
    let theirs = walk(
        ctx,
        &[
            "--no-merges".to_owned(),
            format!("^{}", head.to_hex()),
            up.to_hex(),
        ],
    )?;
    if !theirs.is_empty() {
        // Commits without changes never match.
        let ids = crate::patch_id::CommitIds::new(repo);
        let changing = |list: &[ObjectId]| -> Vec<ObjectId> {
            list.iter()
                .copied()
                .filter(|c| {
                    matches!(repo.read_object(c), Some(Object::Commit(cm))
                        if cm.parents().unwrap_or_default().len() <= 1)
                })
                .filter(|c| ids.header_key(c).is_some_and(|k| !k.is_empty()))
                .collect()
        };
        let same = ids.equivalent(&changing(commits), &changing(&theirs));
        skipped.extend(commits.iter().filter(|c| same.contains(c)));
    }
    Ok(skipped)
}

/// "skipped previously applied commit".
fn warn_skipped(repo: &Repo, c: &ObjectId, quiet: bool) {
    if !quiet {
        eprintln!(
            "warning: {}",
            msg::skipped_cherry_pick(&repo.abbrev(c, repo.default_abbrev()))
        );
    }
}

fn advise_skipped(ctx: &Context, repo: &Repo) -> Result<(), Die> {
    ctx.advisor(&repo.config)?
        .advise(Advice::SkippedCherryPicks, msg::SKIPPED_CHERRY_PICK_HINT)
        .map_err(Die::Fatal)
}

/// `# <subject>` after a picked commit, with the mark of an empty commit
/// (a comment).
fn pick_description(repo: &Repo, commit: &Commit) -> Result<String, Die> {
    let mut arg = format!(
        "# {}",
        lossy(&rgit_commit::message::subject(commit.message()))
    );
    if is_empty_commit(repo, commit) {
        arg.push_str(&format!(
            " {} empty",
            crate::commands::commit::comment_string(repo, b"")?
        ));
    }
    Ok(arg)
}

/// Whether `commit` changes nothing against its first parent.
fn is_empty_commit(repo: &Repo, commit: &Commit) -> bool {
    let tree = commit.tree().ok();
    let parent_tree = match commit.parents().unwrap_or_default().first() {
        Some(p) => crate::switch_branch::commit_tree(repo, p),
        None => Some(ObjectId::empty_tree(repo.hash)),
    };
    tree.is_some() && tree == parent_tree
}

fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<(), Die> {
    std::fs::write(path, data).map_err(io_die)
}

/// Creates `.git/rebase-merge` for a new rebase; `help` is appended to
/// the todo list of an interactive rebase (the user edits it next).
fn write_state(
    repo: &Repo,
    s: &State,
    todo: &todo::Todo,
    help: &str,
    interactive: bool,
) -> Result<(), Die> {
    let dir = state_dir(repo);
    std::fs::create_dir_all(&dir).map_err(io_die)?;
    let f = |name: &str| dir.join(name);
    write(&f("head-name"), format!("{}\n", s.head_name))?;
    write(&f("onto"), format!("{}\n", s.onto.to_hex()))?;
    write(&f("orig-head"), format!("{}\n", s.orig_head.to_hex()))?;
    write(&f("interactive"), "")?;
    if s.quiet {
        write(&f("quiet"), "")?;
    }
    if s.verbose {
        write(&f("verbose"), "")?;
    }
    if s.signoff {
        write(&f("signoff"), "--signoff\n")?;
    }
    if s.cdate_is_adate {
        write(&f("cdate_is_adate"), "")?;
    }
    if s.ignore_date {
        write(&f("ignore_date"), "")?;
    }
    match s.empty {
        Empty::Drop => write(&f("drop_redundant_commits"), "")?,
        Empty::Keep => write(&f("keep_redundant_commits"), "")?,
        Empty::Stop => {}
    }
    if let Some(st) = &s.strategy {
        write(&f("strategy"), format!("{st}\n"))?;
    }
    if !s.xopts.is_empty() {
        let quoted: Vec<String> = s
            .xopts
            .iter()
            .map(|x| format!("\"{}\"", x.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        write(&f("strategy_opts"), format!("{}\n", quoted.join(" ")))?;
    }
    if let Some(a) = &s.autostash {
        write(&f("autostash"), format!("{a}\n"))?;
    }
    if let Some(sign) = &s.gpg_sign {
        write(&f("gpg_sign_opt"), format!("{}\n", sign.to_opt()))?;
    }
    if let Some(sq) = &s.squash_onto {
        write(&f("squash-onto"), format!("{}\n", sq.to_hex()))?;
    }
    if !s.trailers.is_empty() {
        let lines: String = s.trailers.iter().map(|t| format!("{t}\n")).collect();
        write(&f("trailer"), lines)?;
    }
    if s.reschedule_failed_exec {
        write(&f("reschedule-failed-exec"), "")?;
    } else {
        write(&f("no-reschedule-failed-exec"), "")?;
    }
    let text = todo.text();
    write(&f("git-rebase-todo.backup"), text.clone() + help)?;
    if interactive {
        write(&f("git-rebase-todo"), text + help)?;
    } else {
        write(&f("git-rebase-todo"), &text)?;
        write(&f("done"), "")?;
        write(&f("msgnum"), "0\n")?;
        write(&f("end"), format!("{}\n", todo.count()))?;
    }
    Ok(())
}

/// Reads the state of the rebase in progress.
fn read_state(repo: &Repo, dir: &Path) -> Result<State, Die> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    let exists = |name: &str| dir.join(name).exists();
    let oid = |name: &str| read(name).and_then(|s| parse_oid(repo, &s));
    let head_name = read("head-name")
        .map(|s| s.trim_end().to_owned())
        .unwrap_or_else(|| msg::DETACHED_HEAD.to_owned());
    let (Some(onto), Some(orig_head)) = (oid("onto"), oid("orig-head")) else {
        return Err(Die::Fatal(seqmsg::could_not_read(
            &repo
                .setup
                .git_dir
                .join("rebase-merge/onto")
                .to_string_lossy(),
        )));
    };
    let empty = if exists("keep_redundant_commits") {
        Empty::Keep
    } else if exists("drop_redundant_commits") {
        Empty::Drop
    } else {
        Empty::Stop
    };
    let xopts = read("strategy_opts")
        .map(|s| parse_quoted_list(&s))
        .unwrap_or_default();
    let signoff = exists("signoff");
    let cdate_is_adate = exists("cdate_is_adate");
    let ignore_date = exists("ignore_date");
    let trailers: Vec<String> = read("trailer")
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(State {
        head_name,
        onto,
        orig_head,
        quiet: exists("quiet"),
        verbose: exists("verbose"),
        signoff,
        cdate_is_adate,
        ignore_date,
        empty,
        strategy: read("strategy").map(|s| s.trim_end().to_owned()),
        xopts,
        autostash: read("autostash").map(|s| s.trim().to_owned()),
        squash_onto: oid("squash-onto"),
        force: signoff || cdate_is_adate || ignore_date || !trailers.is_empty(),
        reschedule_failed_exec: exists("reschedule-failed-exec"),
        gpg_sign: read("gpg_sign_opt").and_then(|s| Sign::from_opt(s.trim_end_matches('\n'))),
        trailers,
    })
}

fn parse_quoted_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = s.trim_end().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' if !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

// ----------------------------------------------------------------------
// Replaying.

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    /// The rebase stops; the command exits with this code.
    Exit(i32),
    /// The command could not run at all (local changes in the way): it
    /// goes back to the front of the todo list.
    Rescheduled,
}

/// The files of a `squash`/`fixup` chain in progress.
const CURRENT_FIXUPS: &str = "current-fixups";
const MESSAGE_SQUASH: &str = "message-squash";
const SQUASH_FIRST: &str = "message-squash-first";
const REWRITTEN_PENDING: &str = "rewritten-pending";

struct Rebase<'c> {
    ctx: &'c mut Context,
    state: State,
    action: String,
    /// The rebase in progress uses the apply back end (`rebase-apply`).
    apply: bool,
}

impl<'c> Rebase<'c> {
    fn load(ctx: &'c mut Context) -> Result<Rebase<'c>, Die> {
        let repo = Repo::open(ctx, Mode::Required)?;
        repo.work_tree()?;
        let merge_dir = state_dir(&repo);
        let (dir, apply) = if merge_dir.is_dir() {
            (merge_dir, false)
        } else if let Some(dir) = apply_state_dir(&repo) {
            (dir, true)
        } else {
            return Err(Die::Fatal(msg::NO_REBASE_IN_PROGRESS.to_owned()));
        };
        let state = read_state(&repo, &dir)?;
        drop(repo);
        Ok(Rebase {
            ctx,
            state,
            action: reflog_action(),
            apply,
        })
    }

    fn repo(&self) -> Result<Repo, Die> {
        Repo::open(self.ctx, Mode::Required)
    }

    /// "You can amend the commit now", with the signing option.
    fn edit_advice(&self) -> String {
        let sign = match &self.state.gpg_sign {
            Some(s) => sq_quote(s.to_opt().as_bytes()),
            None => String::new(),
        };
        msg::edit_advice(&sign)
    }

    fn dir(&self) -> Result<PathBuf, Die> {
        let repo = self.repo()?;
        Ok(if self.apply {
            git_dir(&repo).join("rebase-apply")
        } else {
            state_dir(&repo)
        })
    }

    fn git_path(&self, name: &str) -> Result<PathBuf, Die> {
        Ok(git_dir(&self.repo()?).join(name))
    }

    fn remove_git_file(&self, name: &str) -> Result<(), Die> {
        let _ = std::fs::remove_file(self.git_path(name)?);
        Ok(())
    }

    /// The remaining todo list; unparsable lines are printed with
    /// `extra`, and give `Err(())`.
    fn read_todo(&self) -> Result<Result<todo::Todo, ()>, Die> {
        let mut repo = self.repo()?;
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let text =
            std::fs::read_to_string(state_dir(&repo).join("git-rebase-todo")).unwrap_or_default();
        let mut resolve = |name: &str| resolve_named(&mut repo, name);
        Ok(match todo::parse(&text, &comment, &mut resolve) {
            Ok(mut t) => {
                t.strip_comments(&comment);
                Ok(t)
            }
            Err(bad) => {
                print_bad(self.ctx, &repo, &bad)?;
                eprintln!("error: {}", msg::PLEASE_FIX_WITH_EDIT_TODO);
                Err(())
            }
        })
    }

    fn count_done(&self) -> Result<usize, Die> {
        let text = std::fs::read_to_string(self.dir()?.join("done")).unwrap_or_default();
        Ok(text
            .lines()
            .filter(|l| {
                let l = l.trim();
                !l.is_empty() && !l.starts_with('#')
            })
            .count())
    }

    /// Runs the commands of `todo`, then finishes the rebase.
    fn run(&mut self, mut todo: todo::Todo) -> CmdResult {
        let dir = self.dir()?;
        let mut done = self.count_done()?;
        loop {
            // Blank and comment lines go to `done` with the next command.
            let mut skipped = Vec::new();
            while let Some(todo::Line::Comment(c)) = todo.lines.first() {
                skipped.push(c.clone());
                todo.lines.remove(0);
            }
            let Some(item) = todo.pop() else {
                break;
            };
            let mut done_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("done"))
                .map_err(io_die)?;
            for c in skipped {
                writeln!(done_file, "{c}").map_err(io_die)?;
            }
            writeln!(done_file, "{}", item.line()).map_err(io_die)?;
            drop(done_file);
            write(&dir.join("git-rebase-todo"), todo.text())?;
            done += 1;
            let total = done + todo.count();
            write(&dir.join("msgnum"), format!("{done}\n"))?;
            write(&dir.join("end"), format!("{total}\n"))?;
            if !self.state.quiet {
                let end = if self.state.verbose { "\n" } else { "\r" };
                eprint!("{}{end}", msg::progress(done, total));
            }
            use todo::Cmd;
            self.remove_git_file("AUTO_MERGE")?;
            let step = match item.cmd {
                Cmd::Pick | Cmd::Reword | Cmd::Edit => self.pick(&item, &todo).map_err(fail_die)?,
                Cmd::Squash | Cmd::Fixup => self.meld(&item, &todo).map_err(fail_die)?,
                Cmd::Exec => self.exec(&item, &mut todo)?,
                Cmd::Break => {
                    let mut repo = self.repo()?;
                    if let Some(h) = sequencer::head_commit(&mut repo) {
                        let subject = match repo.read_object(&h) {
                            Some(Object::Commit(c)) => lossy(sequencer::subject_line(c.message())),
                            _ => String::new(),
                        };
                        let abbrev = repo.abbrev(&h, repo.default_abbrev());
                        eprintln!("{}", msg::stopped_at(&abbrev, &subject));
                    }
                    Step::Exit(0)
                }
                Cmd::Drop | Cmd::Noop => Step::Done,
                Cmd::Label => self.label(&item)?,
                Cmd::Reset => self.reset(&item)?,
                Cmd::Merge => self.merge(&item, &todo).map_err(fail_die)?,
                Cmd::UpdateRef => self.update_ref(&item)?,
            };
            match step {
                Step::Done => {
                    // An editor or `exec` may have edited the list.
                    let now =
                        std::fs::read_to_string(dir.join("git-rebase-todo")).unwrap_or_default();
                    if now != todo.text() {
                        todo = match self.read_todo()? {
                            Ok(t) => t,
                            Err(()) => return Ok(1),
                        };
                    }
                }
                Step::Exit(code) => return Ok(code),
                Step::Rescheduled => {
                    todo.push_front(item);
                    return self.reschedule(&todo);
                }
            }
        }
        self.finish()
    }

    /// Applies the change of `id` onto `head` (a cherry-pick without the
    /// commit): `Ok(None)` when the work tree could not be updated.
    fn apply(
        &mut self,
        repo: &Repo,
        id: &ObjectId,
        commit: &Commit,
        head_tree: ObjectId,
    ) -> Result<Option<rgit_merge::MergeResult>, Fail> {
        let parent = commit.parents().unwrap_or_default().first().copied();
        let empty_tree = ObjectId::empty_tree(repo.hash);
        let abbrev = repo.abbrev(id, repo.default_abbrev());
        let subject = lossy(sequencer::subject_line(commit.message()));
        let label = format!("{abbrev} ({subject})");
        let commit_tree = commit.tree().map_err(|e| Fail::Error(e.to_string()))?;
        let parent_tree = match parent {
            Some(p) => sequencer::tree_of(repo, &p)?,
            None => empty_tree,
        };
        let parent_label = match parent {
            Some(_) => seqmsg::parent_of(&label),
            None => seqmsg::EMPTY_TREE_LABEL.to_owned(),
        };
        if let Some(name) = self
            .state
            .strategy
            .clone()
            .filter(|s| !matches!(s.as_str(), "ort" | "recursive" | "subtree"))
            && let Some(result) = self.custom_strategy(repo, &name, parent, id, head_tree)?
        {
            if result.is_none() {
                // The strategy failed: the pick is rescheduled.
                let subject = lossy(sequencer::subject_line(commit.message()));
                eprintln!("error: {}", seqmsg::could_not_apply(&abbrev, &subject));
                self.ctx
                    .advisor(&repo.config)?
                    .advise(Advice::MergeConflict, msg::CONFLICT_ADVICE)
                    .map_err(Die::Fatal)?;
            }
            return Ok(result);
        }
        let result = sequencer::merge_with(
            repo,
            self.state.strategy.as_deref(),
            &self.state.xopts,
            &parent_tree,
            &head_tree,
            &commit_tree,
            (&parent_label, &label),
        )?;
        if result.tree == empty_tree {
            repo.odb()
                .write(ObjectKind::Tree, b"")
                .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        }
        match sequencer::two_way(repo, head_tree, result.tree, Some(&result), true) {
            Err(Fail::Printed) => return Ok(None),
            r => r?,
        }
        // A clean pick is silent (as in git).
        self.print_merge_messages(repo, &result, !result.clean)?;
        Ok(Some(result))
    }

    /// A pick with a custom strategy: the `git-merge-<name>` program
    /// merges in the index and work tree (`--<option>`s, the parent, `--`,
    /// `HEAD`, the commit). `None` when there is no such program (the
    /// built-in merge reports the strategy), `Some(None)` when the
    /// program failed other than with conflicts.
    fn custom_strategy(
        &mut self,
        repo: &Repo,
        name: &str,
        parent: Option<ObjectId>,
        id: &ObjectId,
        head_tree: ObjectId,
    ) -> Result<Option<Option<rgit_merge::MergeResult>>, Fail> {
        let mut cmd = std::process::Command::new(format!("git-merge-{name}"));
        for x in &self.state.xopts {
            cmd.arg(format!("--{x}"));
        }
        if let Some(p) = parent {
            cmd.arg(p.to_hex());
        }
        cmd.arg("--").arg("HEAD").arg(id.to_hex());
        cmd.envs(self.ctx.child_env());
        let _ = std::io::stdout().flush();
        let code = match cmd.status() {
            Ok(status) => status.code().unwrap_or(2),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Fail::Die(io_die(e))),
        };
        let repo2 = Repo::open(self.ctx, Mode::Required).map_err(Fail::Die)?;
        let (index, _) = repo2.read_index().map_err(Fail::Die)?;
        let conflicts: Vec<rgit_merge::ConflictedPath> = index
            .unmerged()
            .filter_map(|e| e.first())
            .map(|e| rgit_merge::ConflictedPath {
                path: e.path().to_vec(),
                stages: [None, None, None],
            })
            .collect();
        let clean = code == 0 && conflicts.is_empty();
        let tree = if clean {
            crate::commands::write_tree::write_tree(&repo2, &index, false, None)
                .map_err(Fail::Die)?
        } else {
            head_tree
        };
        let _ = (repo, name);
        if code > 1 {
            return Ok(Some(None));
        }
        Ok(Some(Some(rgit_merge::MergeResult {
            tree,
            clean,
            conflicts,
            messages: Vec::new(),
            rename_limit_needed: None,
        })))
    }

    fn print_merge_messages(
        &mut self,
        repo: &Repo,
        result: &rgit_merge::MergeResult,
        show: bool,
    ) -> Result<(), Fail> {
        let mut out = Vec::new();
        for m in result.messages.iter().filter(|_| show) {
            out.extend_from_slice(&m.text);
            out.push(b'\n');
        }
        {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
        crate::commands::merge_tree::advise_submodules(self.ctx, repo, result)?;
        write(
            &git_dir(repo).join("AUTO_MERGE"),
            format!("{}\n", result.tree.to_hex()),
        )?;
        Ok(())
    }

    /// Records a conflicted stop at `id` and prints the advice.
    fn stop_conflicted(
        &mut self,
        repo: &Repo,
        id: &ObjectId,
        result: &rgit_merge::MergeResult,
        message: &[u8],
        author: &Signature,
        rest: Option<&str>,
    ) -> Result<Step, Fail> {
        let comment = crate::commands::commit::comment_string(repo, b"")?;
        let paths: Vec<Vec<u8>> = result.conflicts.iter().map(|c| c.path.clone()).collect();
        let mut m = message.to_vec();
        if rest.is_some() {
            m.extend_from_slice(&sequencer::conflicts_block(
                &comment,
                &paths,
                repo.quote_path(),
            ));
        }
        self.write_stopped(repo, id, &m, author, false)?;
        let abbrev = repo.abbrev(id, repo.default_abbrev());
        let subject = match repo.read_object(id) {
            Some(Object::Commit(c)) => lossy(sequencer::subject_line(c.message())),
            _ => String::new(),
        };
        eprintln!("error: {}", seqmsg::could_not_apply(&abbrev, &subject));
        self.ctx
            .advisor(&repo.config)?
            .advise(Advice::MergeConflict, msg::CONFLICT_ADVICE)
            .map_err(Die::Fatal)?;
        match rest {
            Some(rest) => eprintln!("{}", msg::could_not_apply(&abbrev, rest)),
            None => eprint!("{}", self.edit_advice()),
        }
        Ok(Step::Exit(1))
    }

    /// `pick`, `reword` and `edit`: replays one commit onto `HEAD`.
    fn pick(&mut self, item: &todo::Item, todo: &todo::Todo) -> Result<Step, Fail> {
        self.remove_git_file("REBASE_HEAD")?;
        let mut repo = self.repo()?;
        let id = item.commit.expect("pick has a commit");
        let head = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Fail::Error(seqmsg::could_not_read("HEAD")))?;
        let head_tree = sequencer::tree_of(&repo, &head)?;
        let commit = sequencer::read_commit(&repo, &id)?;
        let parent = commit.parents().unwrap_or_default().first().copied();

        let ff = !self.state.force
            && match parent {
                Some(p) => p == head,
                None => Some(head) == self.state.squash_onto,
            };
        if ff {
            let tree = sequencer::tree_of(&repo, &id)?;
            match sequencer::two_way(&repo, head_tree, tree, None, true) {
                Err(Fail::Printed) => return Ok(Step::Rescheduled),
                r => r?,
            }
            let what = seqmsg::reflog("rebase", seqmsg::FAST_FORWARD);
            sequencer::update_head(self.ctx, &repo, id, Some(head), &what)?;
            return self.after_pick(repo, item, id, todo, true);
        }

        let Some(result) = self.apply(&repo, &id, &commit, head_tree)? else {
            return Ok(Step::Rescheduled);
        };
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let mut message = commit.message().to_vec();
        if self.state.signoff {
            message = sequencer::signoff_message(&repo, &message, &comment)?;
        }
        message = self.add_trailers(&repo, message, &comment)?;
        let original_author = self.author_of(&commit, &id, false)?;
        let author = self.author_of(&commit, &id, self.state.ignore_date)?;
        let subject = lossy(sequencer::subject_line(commit.message()));

        if !result.clean {
            return self.stop_conflicted(
                &repo,
                &id,
                &result,
                &message,
                &original_author,
                Some(&item.arg),
            );
        }

        let commit_tree = commit.tree().map_err(|e| Fail::Error(e.to_string()))?;
        let parent_tree = match parent {
            Some(p) => sequencer::tree_of(&repo, &p)?,
            None => ObjectId::empty_tree(repo.hash),
        };
        if result.tree == head_tree && commit_tree != parent_tree {
            match self.state.empty {
                Empty::Drop => {
                    eprintln!("{}", seqmsg::dropping(&id.to_hex(), &subject));
                    return Ok(Step::Done);
                }
                Empty::Keep => {}
                Empty::Stop => {
                    self.write_stopped(&repo, &id, &message, &original_author, true)?;
                    drop(repo);
                    self.commit_child(&["-n".to_owned(), "--no-edit".to_owned()], &[], true)?;
                    let abbrev = repo_abbrev(self, &id)?;
                    eprintln!("{}", msg::could_not_apply(&abbrev, &item.arg));
                    return Ok(Step::Exit(1));
                }
            }
        }

        // With --root, the empty placeholder commit is not a parent: the
        // first commit becomes a root.
        let parents: &[ObjectId] = if Some(head) == self.state.squash_onto {
            &[]
        } else {
            std::slice::from_ref(&head)
        };
        let new = self.commit(
            &mut repo,
            parents,
            result.tree,
            commit.encoding(),
            &author,
            message.clone(),
            &comment,
            item.cmd.name(),
        )?;
        if new.is_none() {
            // Signing failed: stop like git (with `CHERRY_PICK_HEAD`) so
            // that `--continue` commits.
            self.write_stopped(&repo, &id, &message, &original_author, true)?;
            drop(repo);
            let abbrev = repo_abbrev(self, &id)?;
            eprintln!("{}", msg::could_not_apply(&abbrev, &item.arg));
            return Ok(Step::Exit(1));
        }
        self.after_pick(repo, item, id, todo, false)
    }

    /// After `id` was replayed as `HEAD`: `reword` lets the user edit the
    /// message, `edit` stops.
    fn after_pick(
        &mut self,
        mut repo: Repo,
        item: &todo::Item,
        id: ObjectId,
        todo: &todo::Todo,
        fast_forward: bool,
    ) -> Result<Step, Fail> {
        let new = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Fail::Error(seqmsg::could_not_read("HEAD")))?;
        match item.cmd {
            todo::Cmd::Reword => {
                drop(repo);
                let env = vec![(
                    "GIT_REFLOG_ACTION".to_owned(),
                    format!("{} (reword)", self.action),
                )];
                // The pre-commit and commit-msg hooks run, as in git
                // (rebase --no-verify only skips pre-rebase).
                let args = ["--amend", "--allow-empty", "-e"].map(str::to_owned);
                let code = self.commit_child(&args, &env, false)?;
                self.remove_git_file("AUTO_MERGE")?;
                if code != 0 {
                    return Ok(Step::Exit(1));
                }
                let mut repo = self.repo()?;
                let new = sequencer::head_commit(&mut repo).unwrap_or(new);
                self.record_rewritten(&id, &new, todo.peek())?;
                Ok(Step::Done)
            }
            todo::Cmd::Edit => {
                let commit = sequencer::read_commit(&repo, &id)?;
                let author = self.author_of(&commit, &id, false)?;
                write(
                    &state_dir(&repo).join("amend"),
                    format!("{}\n", new.to_hex()),
                )?;
                let mut message = commit.message().to_vec();
                if fast_forward {
                    message.push(b'\n');
                }
                self.write_stopped(&repo, &id, &message, &author, false)?;
                let _ = std::fs::remove_file(git_dir(&repo).join("MERGE_MSG"));
                let abbrev = repo.abbrev(&id, repo.default_abbrev());
                eprintln!("{}", msg::stopped_at_edit(&abbrev, &item.arg));
                eprint!("{}", self.edit_advice());
                Ok(Step::Exit(0))
            }
            _ => {
                self.record_rewritten(&id, &new, todo.peek())?;
                Ok(Step::Done)
            }
        }
    }

    /// `squash` and `fixup`: melds a commit into `HEAD`.
    fn meld(&mut self, item: &todo::Item, todo: &todo::Todo) -> Result<Step, Fail> {
        self.remove_git_file("REBASE_HEAD")?;
        let mut repo = self.repo()?;
        let id = item.commit.expect("meld has a commit");
        let head = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Fail::Error(seqmsg::could_not_read("HEAD")))?;
        let head_commit = sequencer::read_commit(&repo, &head)?;
        let head_tree = sequencer::tree_of(&repo, &head)?;
        let commit = sequencer::read_commit(&repo, &id)?;
        let dir = state_dir(&repo);

        // The chain so far.
        let mut chain = std::fs::read_to_string(dir.join(CURRENT_FIXUPS)).unwrap_or_default();
        if chain.is_empty() {
            write(&dir.join(SQUASH_FIRST), head_commit.message())?;
        }
        let flag = item.flag.map(|f| format!(" -{f}")).unwrap_or_default();
        chain.push_str(&format!("{}{flag} {}\n", item.cmd.name(), id.to_hex()));
        write(&dir.join(CURRENT_FIXUPS), &chain)?;
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let (template, wants_editor) = self.squash_message(&repo, &comment)?;
        write(&dir.join(MESSAGE_SQUASH), &template)?;
        let last = !todo.peek().is_some_and(|i| i.cmd.melds());

        let Some(result) = self.apply(&repo, &id, &commit, head_tree)? else {
            // Not applied: the chain is as before.
            let before = chain.lines().count() - 1;
            if before == 0 {
                self.end_chain(&repo)?;
            } else {
                let kept: Vec<&str> = chain.lines().take(before).collect();
                write(&dir.join(CURRENT_FIXUPS), kept.join("\n") + "\n")?;
                let (t, _) = self.squash_message(&repo, &comment)?;
                write(&dir.join(MESSAGE_SQUASH), &t)?;
            }
            return Ok(Step::Rescheduled);
        };
        let author = head_commit
            .author()
            .and_then(|a| a.to_owned())
            .ok_or_else(|| Fail::Error(seqmsg::cannot_get_message(&head.to_hex())))?;
        if !result.clean {
            let original = self.author_of(&commit, &id, false)?;
            write(&dir.join("amend"), format!("{}\n", head.to_hex()))?;
            return self.stop_conflicted(&repo, &id, &result, &template, &original, None);
        }

        // A meld that leaves the (non-empty) commit empty stops: `commit
        // --amend` refuses it, and the user decides.
        let head_parent_tree = match head_commit.parents().unwrap_or_default().first() {
            Some(p) => sequencer::tree_of(&repo, p)?,
            None => ObjectId::empty_tree(repo.hash),
        };
        if result.tree == head_parent_tree && head_tree != head_parent_tree {
            let original = self.author_of(&commit, &id, false)?;
            write(&dir.join("amend"), format!("{}\n", head.to_hex()))?;
            self.write_stopped(&repo, &id, &template, &original, false)?;
            drop(repo);
            let args = vec![
                "--amend".to_owned(),
                "-n".to_owned(),
                "--cleanup=strip".to_owned(),
                "-F".to_owned(),
                dir.join(MESSAGE_SQUASH).to_string_lossy().into_owned(),
            ];
            let env = vec![(
                "GIT_REFLOG_ACTION".to_owned(),
                format!("{} ({})", self.action, item.cmd.name()),
            )];
            // git shows the refusal on standard output when the message
            // would be edited (a squash), else on standard error.
            let to_stderr = !(last && wants_editor);
            if self.commit_child(&args, &env, to_stderr)? == 0 {
                // Not refused after all (a hook changed the index).
                let mut repo = self.repo()?;
                let new = sequencer::head_commit(&mut repo).unwrap_or(head);
                self.clear_stop()?;
                if last {
                    self.end_chain(&repo)?;
                }
                self.record_rewritten(&id, &new, todo.peek())?;
                return Ok(Step::Done);
            }
            eprint!("{}", self.edit_advice());
            return Ok(Step::Exit(1));
        }

        if last && wants_editor {
            let gd = git_dir(&repo);
            write(&gd.join("REBASE_HEAD"), format!("{}\n", id.to_hex()))?;
            write(&gd.join("MERGE_MSG"), &template)?;
            drop(repo);
            let env = vec![(
                "GIT_REFLOG_ACTION".to_owned(),
                format!("{} ({})", self.action, item.cmd.name()),
            )];
            let args = vec![
                "--amend".to_owned(),
                "--allow-empty".to_owned(),
                "-n".to_owned(),
                "-e".to_owned(),
                "-F".to_owned(),
                dir.join(MESSAGE_SQUASH).to_string_lossy().into_owned(),
            ];
            let code = self.commit_child(&args, &env, false)?;
            self.remove_git_file("AUTO_MERGE")?;
            self.remove_git_file("MERGE_MSG")?;
            if code != 0 {
                return Ok(Step::Exit(1));
            }
            let mut repo = self.repo()?;
            let new = sequencer::head_commit(&mut repo).unwrap_or(head);
            self.end_chain(&repo)?;
            self.record_rewritten(&id, &new, todo.peek())?;
            return Ok(Step::Done);
        }

        let message = if last {
            let m = rgit_commit::message::cleanup(
                &template,
                rgit_commit::message::Cleanup::Strip.resolve(false),
                &comment,
            );
            // The `--trailer`s go into the final message of the chain.
            self.add_trailers(&repo, m, &comment)?
        } else {
            template
        };
        let parents = head_commit.parents().unwrap_or_default();
        let new = self.commit(
            &mut repo,
            &parents,
            result.tree,
            head_commit.encoding(),
            &author,
            message,
            &comment,
            item.cmd.name(),
        )?;
        let Some(new) = new else {
            return Ok(Step::Exit(1));
        };
        // The commit amended `HEAD`.
        let hooks = crate::hooks::for_repo(self.ctx, &repo.setup, &repo.config)?;
        if hooks.exists("post-rewrite") {
            let data = format!("{} {}\n", head.to_hex(), new.to_hex());
            let mut o = RunOptions::with_args(["amend"]);
            o.stdin = Input::Data(data.as_bytes());
            crate::hooks::run(&hooks, "post-rewrite", &o)?;
        }
        if last {
            self.end_chain(&repo)?;
        }
        self.record_rewritten(&id, &new, todo.peek())?;
        Ok(Step::Done)
    }

    /// A `squash`/`fixup` that stopped is not applied after all (`--skip`,
    /// or `--continue` with nothing staged): it leaves the chain, and when
    /// the chain ends here, the commit gets the message of the rest.
    fn drop_from_chain(&mut self, todo: &todo::Todo, what: &str) -> Result<Option<i32>, Die> {
        let repo = self.repo()?;
        let dir = state_dir(&repo);
        let chain = std::fs::read_to_string(dir.join(CURRENT_FIXUPS)).unwrap_or_default();
        if chain.is_empty() {
            return Ok(None);
        }
        let all: Vec<&str> = chain.lines().collect();
        let kept = &all[..all.len().saturating_sub(1)];
        if kept.is_empty() {
            self.end_chain(&repo)?;
            return Ok(None);
        }
        write(&dir.join(CURRENT_FIXUPS), kept.join("\n") + "\n")?;
        if todo.peek().is_some_and(|i| i.cmd.melds()) {
            return Ok(None);
        }
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let (template, wants_editor) = self.squash_message(&repo, &comment).map_err(fail_die)?;
        write(&dir.join(MESSAGE_SQUASH), &template)?;
        let mut args = vec![
            "--amend".to_owned(),
            "--allow-empty".to_owned(),
            "-n".to_owned(),
        ];
        if wants_editor {
            args.push("-e".to_owned());
        } else {
            args.push("--cleanup=strip".to_owned());
        }
        args.push("-F".to_owned());
        args.push(dir.join(MESSAGE_SQUASH).to_string_lossy().into_owned());
        drop(repo);
        let env = vec![(
            "GIT_REFLOG_ACTION".to_owned(),
            format!("{} ({what})", self.action),
        )];
        let code = self.commit_child(&args, &env, true)?;
        if code != 0 {
            return Ok(Some(code));
        }
        let repo = self.repo()?;
        self.end_chain(&repo)?;
        Ok(None)
    }

    /// Forgets the `squash`/`fixup` chain.
    fn end_chain(&self, repo: &Repo) -> Result<(), Die> {
        let dir = state_dir(repo);
        for f in [CURRENT_FIXUPS, MESSAGE_SQUASH, SQUASH_FIRST] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        Ok(())
    }

    /// The message of the `squash`/`fixup` chain in progress, and whether
    /// the user edits it at the end.
    fn squash_message(&self, repo: &Repo, comment: &str) -> Result<(Vec<u8>, bool), Fail> {
        let dir = state_dir(repo);
        let first = std::fs::read(dir.join(SQUASH_FIRST)).unwrap_or_default();
        let chain = std::fs::read_to_string(dir.join(CURRENT_FIXUPS)).unwrap_or_default();
        let mut members: Vec<(todo::Cmd, Option<char>, Vec<u8>)> = Vec::new();
        for line in chain.lines() {
            let mut words = line.split(' ');
            let cmd = match words.next() {
                Some("squash") => todo::Cmd::Squash,
                _ => todo::Cmd::Fixup,
            };
            let mut next = words.next().unwrap_or_default();
            let mut flag = None;
            if let Some(f) = next.strip_prefix('-') {
                flag = f.chars().next();
                next = words.next().unwrap_or_default();
            }
            let Some(id) = parse_oid(repo, next) else {
                continue;
            };
            let commit = sequencer::read_commit(repo, &id)?;
            members.push((cmd, flag, commit.message().to_vec()));
        }
        // --signoff: the replaced messages carry the trailer.
        if self.state.signoff {
            let n = members.len();
            for (k, m) in members.iter_mut().enumerate() {
                if m.1.is_some() && k + 1 < n {
                    m.2 = sequencer::signoff_message(repo, &m.2, comment)?;
                }
            }
        }
        Ok(squash_template(comment, &first, &members))
    }

    /// `exec`: runs a shell command; stops when it fails or leaves
    /// changes behind.
    fn exec(&mut self, item: &todo::Item, todo: &mut todo::Todo) -> Result<Step, Die> {
        let cmd = item.arg.as_str();
        if !self.state.quiet {
            eprintln!("{}", msg::executing(cmd));
        }
        let _ = std::io::stdout().flush();
        let mut repo = self.repo()?;
        let work_tree = repo.work_tree()?;
        // A plain program runs directly, anything else through the shell.
        let plain = !cmd.is_empty()
            && cmd
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./+,:@".contains(&b));
        let mut command = if plain {
            std::process::Command::new(cmd)
        } else {
            let mut c = std::process::Command::new(rgit_ident::editor::SHELL_PATH);
            c.arg("-c").arg(cmd);
            c
        };
        command.current_dir(&work_tree).envs(self.ctx.child_env());
        let ok = match command.status() {
            Ok(s) => s.success(),
            Err(e) => {
                let reason = e.to_string();
                let reason = match reason.find(" (os error") {
                    Some(i) => reason[..i].to_owned(),
                    None => reason,
                };
                eprintln!("error: {}", msg::cannot_run(cmd, &reason));
                false
            }
        };
        if !ok {
            eprintln!("warning: {}", msg::execution_failed(cmd));
            eprintln!("{}", msg::EXEC_FAILED_ADVICE);
            if self.state.reschedule_failed_exec {
                todo.push_front(item.clone());
                write(&state_dir(&repo).join("git-rebase-todo"), todo.text())?;
                let advisor = self.ctx.advisor(&repo.config)?;
                eprint!("{}", advisor.hint(&msg::rescheduled(&item.line())));
            }
            return Ok(Step::Exit(1));
        }
        let (unstaged, staged) = local_changes(&mut repo)?;
        if unstaged || staged {
            eprintln!(
                "error: {}",
                if unstaged {
                    msg::UNSTAGED_CHANGES
                } else {
                    msg::UNCOMMITTED_CHANGES
                }
            );
            eprintln!("warning: {}", msg::execution_left_changes(cmd));
            return Ok(Step::Exit(1));
        }
        Ok(Step::Done)
    }

    /// `label <name>`: `refs/rewritten/<name>` at `HEAD`.
    fn label(&mut self, item: &todo::Item) -> Result<Step, Die> {
        let mut repo = self.repo()?;
        let name = item.arg.split([' ', '\t']).next().unwrap_or_default();
        let full = format!("refs/rewritten/{name}");
        let Some(head) = sequencer::head_commit(&mut repo) else {
            return Err(Die::Fatal(seqmsg::could_not_read("HEAD")));
        };
        if !rgit_refs::name::is_valid_refname(full.as_bytes(), Default::default()) {
            eprintln!("error: {}", msg::invalid_label(name));
            return Ok(Step::Rescheduled);
        }
        let old = repo.refs.resolve(&full);
        let update = match old {
            Some(o) => RefUpdate::update(full.as_str(), head, Some(o)),
            None => RefUpdate::create(full.as_str(), head),
        };
        let what = msg::reflog(&self.action, "label", name);
        ref_tx(self.ctx, &repo, update.message(what), false)?;
        Ok(Step::Done)
    }

    /// The commit a `reset` or `merge` names: a label or a revision.
    fn resolve_label(&mut self, repo: &mut Repo, name: &str) -> Option<ObjectId> {
        if name == "[new root]" {
            // Without `--root` the placeholder is made when first needed.
            if self.state.squash_onto.is_none() {
                let id = squash_onto_commit(repo).ok()?;
                write(
                    &state_dir(repo).join("squash-onto"),
                    format!("{}\n", id.to_hex()),
                )
                .ok()?;
                self.state.squash_onto = Some(id);
            }
            return self.state.squash_onto;
        }
        if let Some(id) = repo.refs.resolve(&format!("refs/rewritten/{name}")) {
            return Some(id);
        }
        let id = repo.resolve(name)?;
        let commit = repo.peel_to(id, ObjectKind::Commit);
        if commit.is_none()
            && let Some(kind) = repo.object_kind(&id)
        {
            eprintln!("error: {}", msg::not_a_commit(&id.to_hex(), kind.as_str()));
        }
        commit
    }

    /// `reset <label>`: moves `HEAD`, the index and the work tree.
    fn reset(&mut self, item: &todo::Item) -> Result<Step, Die> {
        let mut repo = self.repo()?;
        let name = if item.arg.starts_with("[new root]") {
            "[new root]"
        } else {
            item.arg.split([' ', '\t']).next().unwrap_or_default()
        };
        let Some(target) = self.resolve_label(&mut repo, name) else {
            eprintln!("error: {}", msg::could_not_resolve(name));
            return Ok(Step::Rescheduled);
        };
        let head = sequencer::head_commit(&mut repo);
        let old_tree = match head {
            Some(h) => sequencer::tree_of(&repo, &h).map_err(fail_die)?,
            None => ObjectId::empty_tree(repo.hash),
        };
        let new_tree = sequencer::tree_of(&repo, &target).map_err(fail_die)?;
        match sequencer::two_way(&repo, old_tree, new_tree, None, true) {
            Err(Fail::Printed) => return Ok(Step::Rescheduled),
            r => r.map_err(fail_die)?,
        }
        let what = msg::reflog(&self.action, "reset", &msg::reset_label(name));
        detach_head(self.ctx, &repo, target, &what)?;
        Ok(Step::Done)
    }

    /// `merge [-C <commit> | -c <commit>] <label> [# <oneline>]`.
    fn merge(&mut self, item: &todo::Item, todo: &todo::Todo) -> Result<Step, Fail> {
        let mut repo = self.repo()?;
        let (labels, oneline) = match item.arg.split_once('#') {
            Some((l, o)) => (l.trim(), Some(o.trim())),
            None => (item.arg.trim(), None),
        };
        let names: Vec<&str> = labels.split_whitespace().collect();
        let mut targets = Vec::new();
        for name in &names {
            let Some(target) = self.resolve_label(&mut repo, name) else {
                eprintln!("error: {}", msg::could_not_resolve(name));
                eprintln!("error: {}", msg::unable_to_parse(name));
                return Ok(Step::Rescheduled);
            };
            targets.push(target);
        }
        let name = names[0];
        let target = targets[0];
        let head = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Fail::Error(seqmsg::could_not_read("HEAD")))?;
        let original = match item.commit {
            Some(c) => Some(sequencer::read_commit(&repo, &c)?),
            None => None,
        };
        let mut wanted = vec![head];
        wanted.extend(&targets);
        // The original merge with the same parents is reused.
        let reuse = item.commit.filter(|_| {
            !self.state.force
                && original
                    .as_ref()
                    .is_some_and(|o| o.parents().unwrap_or_default() == wanted)
        });
        // A merge into the placeholder of `[new root]` is a fast-forward.
        let into_root = (Some(head) == self.state.squash_onto && targets.len() == 1)
            .then_some(target)
            .filter(|_| reuse.is_none());
        if let Some(c) = reuse.or(into_root) {
            let head_tree = sequencer::tree_of(&repo, &head)?;
            let tree = sequencer::tree_of(&repo, &c)?;
            match sequencer::two_way(&repo, head_tree, tree, None, true) {
                Err(Fail::Printed) => return Ok(Step::Rescheduled),
                r => r?,
            }
            let what = seqmsg::reflog("rebase", seqmsg::FAST_FORWARD);
            sequencer::update_head(self.ctx, &repo, c, Some(head), &what)?;
            let mut new = c;
            if item.flag == Some('c') {
                // `-c`: the user rewords the reused merge.
                drop(repo);
                let code = self.reword_merge()?;
                if code != 0 {
                    return Ok(Step::Exit(1));
                }
                let mut repo = self.repo()?;
                new = sequencer::head_commit(&mut repo).unwrap_or(new);
            }
            if let Some(orig) = item.commit {
                self.record_rewritten(&orig, &new, todo.peek())?;
            }
            return Ok(Step::Done);
        }
        // Nothing to merge when every head is already part of `HEAD`.
        let mut merged = true;
        for t in &targets {
            merged &= is_ancestor(&repo, t, &head).map_err(Fail::Die)?;
        }
        let message: Vec<u8> = match &original {
            Some(o) => o.message().to_vec(),
            None => match oneline.filter(|o| !o.is_empty()) {
                Some(o) => o.as_bytes().to_vec(),
                None if names.len() > 1 => msg::merge_branches(&names.join(" ")).into_bytes(),
                None => msg::merge_branch(name).into_bytes(),
            },
        };
        let author = match &original {
            Some(o) => o.author().and_then(|a| a.to_owned()),
            None => None,
        };
        let mut author = match author {
            Some(a) => a,
            None => sequencer::ident(&repo, Role::Author)?,
        };
        if self.state.ignore_date {
            author.time = now();
        }
        if merged {
            // git leaves the message of a new merge it did not make.
            if item.commit.is_none() {
                write(&git_dir(&repo).join("MERGE_MSG"), &message)?;
            }
            return Ok(Step::Done);
        }
        let custom = self
            .state
            .strategy
            .as_deref()
            .is_some_and(|s| !matches!(s, "ort" | "recursive"));
        if targets.len() > 1 || custom {
            drop(repo);
            return self.merge_child(item, todo, &targets, &message, &author);
        }
        let head_tree = sequencer::tree_of(&repo, &head)?;
        let mut opts = rgit_merge::Options {
            branch1: b"HEAD".to_vec(),
            branch2: name.as_bytes().to_vec(),
            style: crate::commands::merge_file::config_style(&repo.config)?,
            allow_unrelated_histories: true,
            ..rgit_merge::Options::default()
        };
        crate::commands::merge_tree::apply_rename_config(&repo.config, &mut opts);
        for x in &self.state.xopts {
            let _ = crate::commands::merge_tree::apply_strategy_option(&mut opts, x);
        }
        let result = {
            let commits = commits_of(&repo);
            rgit_merge::merge_commits(repo.odb(), &commits, &head, &target, &opts)
                .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?
        };
        match sequencer::two_way(&repo, head_tree, result.tree, Some(&result), true) {
            Err(Fail::Printed) => return Ok(Step::Rescheduled),
            r => r?,
        }
        self.print_merge_messages(&repo, &result, true)?;
        let gd = git_dir(&repo);
        if !result.clean {
            write(&gd.join("MERGE_HEAD"), target.to_hex())?;
            self.write_merge_stop(&repo, item, &message, &author)?;
            if let Some(c) = item.commit {
                // git records the merge as rewritten to the old `HEAD`.
                self.record_rewritten(&c, &head, None)?;
            }
            let label = match item.commit {
                Some(c) => repo.abbrev(&c, repo.default_abbrev()),
                None => name.to_owned(),
            };
            eprintln!("{}", msg::could_not_apply(&label, &item.arg));
            return Ok(Step::Exit(1));
        }
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let new = self.commit(
            &mut repo,
            &[head, target],
            result.tree,
            original.as_ref().and_then(|o| o.encoding()),
            &author,
            message,
            &comment,
            "merge",
        )?;
        let Some(mut new) = new else {
            return Ok(Step::Exit(1));
        };
        if item.commit.is_none() || item.flag == Some('c') {
            // A new merge, or `-c`: the user edits the message.
            drop(repo);
            let code = self.reword_merge()?;
            self.remove_git_file("AUTO_MERGE")?;
            if code != 0 {
                return Ok(Step::Exit(1));
            }
            let mut repo = self.repo()?;
            new = sequencer::head_commit(&mut repo).unwrap_or(new);
        }
        if let Some(c) = item.commit {
            self.record_rewritten(&c, &new, todo.peek())?;
        }
        Ok(Step::Done)
    }

    /// Lets the user edit the message of the merge just made (`HEAD`).
    fn reword_merge(&self) -> Result<i32, Die> {
        let env = vec![(
            "GIT_REFLOG_ACTION".to_owned(),
            format!("{} (merge)", self.action),
        )];
        let args = ["--amend", "--allow-empty", "-n", "-e"].map(str::to_owned);
        self.commit_child(&args, &env, false)
    }

    /// `--trailer`: the trailers added to a replayed commit's message (as
    /// `interpret-trailers --trailer` does, following `trailer.*`).
    fn add_trailers(&self, repo: &Repo, message: Vec<u8>, comment: &str) -> Result<Vec<u8>, Fail> {
        if self.state.trailers.is_empty() {
            return Ok(message);
        }
        let conf = sequencer::trailer_config(repo)?;
        let opts = rgit_trailer::Options {
            comment: comment.as_bytes().into(),
            ..rgit_trailer::Options::default()
        };
        let args: Vec<rgit_trailer::NewTrailer> = self
            .state
            .trailers
            .iter()
            .map(|t| rgit_trailer::NewTrailer::new(t.as_bytes()))
            .collect();
        let mut on_error = |m: &str| eprintln!("error: {m}");
        Ok(rgit_trailer::process(&message, &conf, &opts, &args, &mut on_error).to_vec())
    }

    /// The state of a `merge` that stopped with conflicts.
    fn write_merge_stop(
        &self,
        repo: &Repo,
        item: &todo::Item,
        message: &[u8],
        author: &Signature,
    ) -> Result<(), Die> {
        match item.commit {
            Some(c) => self.write_stopped(repo, &c, message, author, false),
            None => {
                let dir = state_dir(repo);
                write(&git_dir(repo).join("MERGE_MSG"), message)?;
                write(&dir.join("message"), message)?;
                write(&dir.join("author-script"), author_script(author))
            }
        }
    }

    /// An octopus `merge`, or one with a strategy rgit's merge does not
    /// implement itself: done by `merge -s <strategy>` as a child, with
    /// the original author.
    fn merge_child(
        &mut self,
        item: &todo::Item,
        todo: &todo::Todo,
        targets: &[ObjectId],
        message: &[u8],
        author: &Signature,
    ) -> Result<Step, Fail> {
        let repo = self.repo()?;
        let dir = state_dir(&repo);
        let gd = git_dir(&repo);
        let edit = item.commit.is_none() || item.flag == Some('c');
        let mut text = message.to_vec();
        // Like git, a line break separates the message from the help
        // below it in the editor.
        if edit || !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        write(&dir.join("message"), text)?;
        let strategy = self
            .state
            .strategy
            .clone()
            .unwrap_or_else(|| if targets.len() > 1 { "octopus" } else { "ort" }.to_owned());
        let mut args: Vec<String> = [
            "merge",
            if edit { "--edit" } else { "--no-edit" },
            "--no-ff",
            "--no-log",
            "--no-stat",
            "--no-verify",
            "--allow-unrelated-histories",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(match &self.state.gpg_sign {
            Some(sign) => sign.to_opt(),
            None => "--no-gpg-sign".to_owned(),
        });
        args.push(format!("--strategy={strategy}"));
        for x in &self.state.xopts {
            args.push(format!("-X{x}"));
        }
        args.push("-F".to_owned());
        args.push(dir.join("message").to_string_lossy().into_owned());
        args.extend(targets.iter().map(ObjectId::to_hex));
        let mut env: Vec<(String, String)> = vec![
            ("GIT_AUTHOR_NAME".into(), lossy(&author.name)),
            ("GIT_AUTHOR_EMAIL".into(), lossy(&author.email)),
            ("GIT_AUTHOR_DATE".into(), format!("@{}", author.time)),
        ];
        if self.state.cdate_is_adate {
            env.push(("GIT_COMMITTER_DATE".into(), format!("@{}", author.time)));
        }
        drop(repo);
        let _ = std::io::stdout().flush();
        let exe = std::env::current_exe().map_err(io_die)?;
        let status = std::process::Command::new(exe)
            .args(&args)
            .envs(self.ctx.child_env())
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .status()
            .map_err(io_die)?;
        if !status.success() {
            if !gd.join("MERGE_HEAD").exists() {
                return Ok(Step::Rescheduled);
            }
            let repo = self.repo()?;
            self.write_merge_stop(&repo, item, message, author)?;
            let label = match item.commit {
                Some(c) => repo.abbrev(&c, repo.default_abbrev()),
                None => item
                    .arg
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            };
            eprintln!("{}", msg::could_not_apply(&label, &item.arg));
            return Ok(Step::Exit(1));
        }
        let mut repo = self.repo()?;
        if let (Some(c), Some(new)) = (item.commit, sequencer::head_commit(&mut repo)) {
            self.record_rewritten(&c, &new, todo.peek())?;
        }
        Ok(Step::Done)
    }

    /// `update-ref <ref>`: the ref is set to `HEAD` at the end.
    fn update_ref(&mut self, item: &todo::Item) -> Result<Step, Die> {
        let mut repo = self.repo()?;
        let Some(head) = sequencer::head_commit(&mut repo) else {
            return Err(Die::Fatal(seqmsg::could_not_read("HEAD")));
        };
        update_refs::record(&repo, &item.arg, head)?;
        Ok(Step::Done)
    }

    /// The author of the replayed commit (`--reset-author-date`).
    fn author_of(
        &self,
        commit: &Commit,
        id: &ObjectId,
        reset_date: bool,
    ) -> Result<Signature, Fail> {
        let mut author = commit
            .author()
            .and_then(|a| a.to_owned())
            .ok_or_else(|| Fail::Error(seqmsg::cannot_get_message(&id.to_hex())))?;
        if reset_date {
            author.time = now();
        }
        Ok(author)
    }

    /// Records a stop at `id`: the message, author, patch and
    /// `REBASE_HEAD` (and `CHERRY_PICK_HEAD` for an empty result).
    fn write_stopped(
        &self,
        repo: &Repo,
        id: &ObjectId,
        message: &[u8],
        author: &Signature,
        empty: bool,
    ) -> Result<(), Die> {
        let dir = state_dir(repo);
        let gd = git_dir(repo);
        write(&dir.join("message"), message)?;
        write(&gd.join("MERGE_MSG"), message)?;
        write(&dir.join("author-script"), author_script(author))?;
        let hex = format!("{}\n", id.to_hex());
        write(&dir.join("stopped-sha"), &hex)?;
        write(&gd.join("REBASE_HEAD"), &hex)?;
        if empty {
            write(&gd.join("CHERRY_PICK_HEAD"), &hex)?;
        }
        let parent = match repo.read_object(id) {
            Some(Object::Commit(c)) => c.parents().unwrap_or_default().first().map(|p| p.to_hex()),
            _ => None,
        }
        .unwrap_or_else(|| ObjectId::empty_tree(repo.hash).to_hex());
        let (_, patch) = run_child(
            self.ctx,
            &["diff", "--no-color", "--no-ext-diff", &parent, &id.to_hex()],
            true,
        )?;
        write(&dir.join("patch"), patch)?;
        Ok(())
    }

    /// Creates a commit and moves `HEAD` to it: hooks `prepare-commit-msg`
    /// and `post-commit`, the reflog entry `rebase (<what>): <subject>`.
    /// `None`: signing failed (the error has been printed).
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &mut self,
        repo: &mut Repo,
        parents: &[ObjectId],
        tree: ObjectId,
        encoding: Option<&[u8]>,
        author: &Signature,
        message: Vec<u8>,
        comment: &str,
        what: &str,
    ) -> Result<Option<ObjectId>, Fail> {
        let head = sequencer::head_commit(repo);
        let mut committer = sequencer::ident(repo, Role::Committer)?;
        if self.state.cdate_is_adate {
            committer.time = author.time;
        }
        let hooks = crate::hooks::for_repo(self.ctx, &repo.setup, &repo.config)?;
        let message = if hooks.exists("prepare-commit-msg") {
            let editmsg = git_dir(repo).join("COMMIT_EDITMSG");
            write(&editmsg, &message)?;
            let shown = repo.setup.git_dir.join("COMMIT_EDITMSG");
            let shown = shown.to_string_lossy().into_owned();
            let mut o = RunOptions::with_args([shown.as_str(), "message"]);
            o.env.push(("GIT_EDITOR".into(), ":".into()));
            let _ = std::io::stdout().flush();
            let outcome = crate::hooks::run(&hooks, "prepare-commit-msg", &o)?;
            if !outcome.ok() {
                return Err(Fail::Printed);
            }
            let edited = std::fs::read(&editmsg).map_err(io_die)?;
            rgit_commit::message::cleanup(
                &edited,
                rgit_commit::message::Cleanup::Verbatim.resolve(false),
                comment,
            )
        } else {
            message
        };
        // `commit.cleanup` applies to the replayed messages.
        let message = match repo
            .config
            .string("commit.cleanup")
            .ok()
            .flatten()
            .map(|v| v.to_string())
            .as_deref()
        {
            Some("strip") => rgit_commit::message::cleanup(
                &message,
                rgit_commit::message::Cleanup::Strip.resolve(false),
                comment,
            ),
            Some("whitespace") => rgit_commit::message::cleanup(
                &message,
                rgit_commit::message::Cleanup::Whitespace.resolve(false),
                comment,
            ),
            _ => message,
        };
        let new = rgit_commit::NewCommit {
            tree,
            parents,
            author,
            committer: &committer,
            encoding,
            message: &message,
        };
        let mut object = new.to_commit();
        if let Some(sign) = &self.state.gpg_sign {
            match signing::sign_commit(&repo.config, repo.hash, &mut object, sign, &committer) {
                Ok(()) => {}
                Err(SignError::Failed) => {
                    eprintln!(
                        "error: {}",
                        crate::messages::signing::FAILED_TO_WRITE_COMMIT
                    );
                    return Ok(None);
                }
                Err(SignError::Die(d)) => return Err(Fail::Die(d)),
            }
        }
        let oid = repo
            .odb()
            .write(ObjectKind::Commit, &object.serialize())
            .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        let subject = lossy(rgit_commit::message::first_line(&message));
        let what = msg::reflog(&self.action, what, &subject);
        sequencer::update_head(self.ctx, repo, oid, head, &what)?;
        let o = RunOptions::with_args(std::iter::empty::<&str>());
        crate::hooks::run(&hooks, "post-commit", &o)?;
        Ok(Some(oid))
    }

    /// Runs `git commit` with `args` (and the environment `env`); with
    /// `to_stderr` its standard output goes to standard error.
    fn commit_child(
        &self,
        args: &[String],
        env: &[(String, String)],
        to_stderr: bool,
    ) -> Result<i32, Die> {
        let _ = std::io::stdout().flush();
        let exe = std::env::current_exe().map_err(io_die)?;
        let sign = match &self.state.gpg_sign {
            Some(sign) => sign.to_opt(),
            None => "--no-gpg-sign".to_owned(),
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("commit")
            .arg(sign)
            .args(args)
            .envs(self.ctx.child_env())
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        // `--committer-date-is-author-date`: the date of the author the
        // commit gets (the amended commit's, else `author-script`'s).
        if self.state.cdate_is_adate
            && let Some(date) = self.author_date_for(args.iter().any(|a| a == "--amend"))
        {
            cmd.env("GIT_COMMITTER_DATE", date);
        }
        if to_stderr {
            let out = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .output()
                .map_err(io_die)?;
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(&out.stdout);
            let _ = e.flush();
            return Ok(out.status.code().unwrap_or(128));
        }
        let status = cmd.status().map_err(io_die)?;
        Ok(status.code().unwrap_or(128))
    }

    /// The author date of the commit `commit_child` makes: `HEAD`'s with
    /// `--amend`, else the one in `author-script`.
    fn author_date_for(&self, amend: bool) -> Option<String> {
        let mut repo = self.repo().ok()?;
        if amend {
            let head = sequencer::head_commit(&mut repo)?;
            let Some(Object::Commit(c)) = repo.read_object(&head) else {
                return None;
            };
            let a = c.author()?.to_owned()?;
            return Some(format!("@{}", a.time));
        }
        if self.state.ignore_date {
            // The author date is now, so is the committer's.
            return None;
        }
        let dir = state_dir(&repo);
        read_author_script(&dir.join("author-script"))
            .into_iter()
            .find(|(k, _)| k == "GIT_AUTHOR_DATE")
            .map(|(_, v)| v)
    }

    /// Puts the command that could not run back on the todo list.
    fn reschedule(&mut self, todo: &todo::Todo) -> CmdResult {
        let repo = self.repo()?;
        let dir = state_dir(&repo);
        write(&dir.join("git-rebase-todo"), todo.text())?;
        let item = todo.peek().expect("rescheduled command").clone();
        if let Some(c) = item.commit
            && (item.cmd.picks() || item.cmd == todo::Cmd::Merge)
        {
            write(
                &git_dir(&repo).join("REBASE_HEAD"),
                format!("{}\n", c.to_hex()),
            )?;
            if let Some(Object::Commit(cm)) = repo.read_object(&c)
                && let Some(a) = cm.author().and_then(|a| a.to_owned())
            {
                write(&dir.join("author-script"), author_script(&a))?;
            }
        }
        let advisor = self.ctx.advisor(&repo.config)?;
        eprint!("{}", advisor.hint(&msg::rescheduled(&item.line())));
        Ok(1)
    }

    /// Records that `old` became `new`; while a `squash`/`fixup` chain
    /// goes on (`next` melds) the old commits wait for the final one.
    fn record_rewritten(
        &self,
        old: &ObjectId,
        new: &ObjectId,
        next: Option<&todo::Item>,
    ) -> Result<(), Die> {
        let dir = self.dir()?;
        let pending_path = dir.join(REWRITTEN_PENDING);
        if next.is_some_and(|i| i.cmd.melds()) {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&pending_path)
                .map_err(io_die)?;
            return writeln!(f, "{}", old.to_hex()).map_err(io_die);
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("rewritten-list"))
            .map_err(io_die)?;
        let pending = std::fs::read_to_string(&pending_path).unwrap_or_default();
        for p in pending.lines().filter(|l| !l.is_empty()) {
            writeln!(f, "{p} {}", new.to_hex()).map_err(io_die)?;
        }
        let _ = std::fs::remove_file(&pending_path);
        writeln!(f, "{} {}", old.to_hex(), new.to_hex()).map_err(io_die)
    }

    /// Updates the branch, runs `post-rewrite`, removes the state and
    /// applies the autostash.
    fn finish(&mut self) -> CmdResult {
        let mut repo = self.repo()?;
        let head = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Die::Fatal(seqmsg::could_not_read("HEAD")))?;
        let head_name = self.state.head_name.clone();
        let dir = state_dir(&repo);
        // The refs of `update-ref` commands move after the branch.
        let ref_updates = update_refs::read(&repo);
        if head_name.starts_with("refs/") {
            let old = repo.refs.resolve(&head_name);
            let what = msg::reflog(
                &self.action,
                "finish",
                &msg::finish_onto(&head_name, &self.state.onto.to_hex()),
            );
            let update = match old {
                Some(o) => RefUpdate::update(head_name.as_str(), head, Some(o)),
                None => RefUpdate::create(head_name.as_str(), head),
            };
            ref_tx(self.ctx, &repo, update.message(what), false)?;
            let what = msg::reflog(&self.action, "finish", &msg::returning_to(&head_name));
            attach_head(self.ctx, &repo, &head_name, &what)?;
        }
        if self.state.verbose {
            let (from, to) = (self.state.orig_head.to_hex(), head.to_hex());
            run_child(self.ctx, &["diff", "--stat", &from, &to], false)?;
        }
        let rewritten = std::fs::read(dir.join("rewritten-list")).unwrap_or_default();
        if !rewritten.is_empty() {
            crate::notes::rewrite_list(
                self.ctx,
                "rebase",
                &rewritten,
                &crate::messages::notes::added_by("notes copy"),
            )?;
            let hooks = crate::hooks::for_repo(self.ctx, &repo.setup, &repo.config)?;
            if hooks.exists("post-rewrite") {
                let mut o = RunOptions::with_args(["rebase"]);
                o.stdin = Input::Data(&rewritten);
                crate::hooks::run(&hooks, "post-rewrite", &o)?;
            }
        }
        delete_labels(self.ctx, &repo)?;
        let applied = update_refs::apply(self.ctx, &repo, &ref_updates);
        // A ref that could not be updated keeps the state (as git does).
        if applied.ok() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        drop(repo);
        if let Some(s) = self.state.autostash.clone() {
            apply_autostash(self.ctx, &s)?;
        }
        if !self.state.quiet {
            eprintln!("{}", msg::success(&head_name));
        }
        applied.report();
        Ok(if applied.ok() { 0 } else { 1 })
    }

    /// Forgets the stop: the pseudo-refs and the stopped commit's files.
    fn clear_stop(&self) -> Result<(), Die> {
        for f in ["REBASE_HEAD", "MERGE_MSG", "CHERRY_PICK_HEAD", "MERGE_HEAD"] {
            self.remove_git_file(f)?;
        }
        let dir = self.dir()?;
        for f in ["message", "author-script", "stopped-sha", "patch", "amend"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        Ok(())
    }
}

/// The abbreviated name of `id`.
fn repo_abbrev(rb: &Rebase<'_>, id: &ObjectId) -> Result<String, Die> {
    let repo = rb.repo()?;
    Ok(repo.abbrev(id, repo.default_abbrev()))
}

/// Removes the labels (`refs/rewritten/*`) of `--rebase-merges`.
fn delete_labels(ctx: &Context, repo: &Repo) -> Result<(), Die> {
    for (name, id) in repo.refs.list("refs/rewritten/") {
        ref_tx(
            ctx,
            repo,
            RefUpdate::delete(name.as_str(), repo.hash, Some(id)),
            false,
        )?;
    }
    Ok(())
}

/// The message of a `squash`/`fixup` chain: `first` is the message of
/// the commit the others meld into; `members` the melded commits with
/// their command, `-C`/`-c` flag and message. Also whether the user
/// edits it (a `squash` or `fixup -c` took part).
fn squash_template(
    comment: &str,
    first: &[u8],
    members: &[(todo::Cmd, Option<char>, Vec<u8>)],
) -> (Vec<u8>, bool) {
    let n = members.len() + 1;
    let mut shown = vec![true; n];
    let mut squash_seen = false;
    let mut edit = false;
    for (k, (cmd, flag, _)) in members.iter().enumerate() {
        let i = k + 1;
        match (cmd, flag) {
            (todo::Cmd::Squash, _) => {
                squash_seen = true;
                edit = true;
            }
            (_, None) => shown[i] = false,
            (_, Some(f)) => {
                if *f == 'c' {
                    edit = true;
                }
                if !squash_seen {
                    for s in shown.iter_mut().take(i) {
                        *s = false;
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let line = |out: &mut Vec<u8>, text: &str| {
        out.extend_from_slice(comment.as_bytes());
        out.push(b' ');
        out.extend_from_slice(text.as_bytes());
        out.push(b'\n');
    };
    line(&mut out, &msg::combination(n));
    for i in 0..n {
        let message: &[u8] = if i == 0 { first } else { &members[i - 1].2 };
        let header = match (i, shown[i]) {
            (0, true) => msg::FIRST_MESSAGE.to_owned(),
            (0, false) => msg::FIRST_SKIPPED.to_owned(),
            (_, true) => msg::nth_message(i + 1),
            (_, false) => msg::nth_skipped(i + 1),
        };
        if i > 0 {
            out.push(b'\n');
        }
        line(&mut out, &header);
        out.push(b'\n');
        let mut body = message.to_vec();
        if !body.ends_with(b"\n") {
            body.push(b'\n');
        }
        if shown[i] {
            let marked = i > 0
                && [&b"squash! "[..], b"fixup! ", b"amend! "]
                    .iter()
                    .any(|p| body.starts_with(p));
            if marked {
                let end = body.iter().position(|&b| b == b'\n').unwrap_or(body.len());
                out.extend_from_slice(comment.as_bytes());
                out.push(b' ');
                out.extend_from_slice(&body[..end]);
                out.extend_from_slice(&body[end..]);
            } else {
                out.extend_from_slice(&body);
            }
        } else {
            for l in body.split_inclusive(|&b| b == b'\n') {
                out.extend_from_slice(comment.as_bytes());
                if l != b"\n" {
                    out.push(b' ');
                }
                out.extend_from_slice(l);
            }
        }
    }
    (out, edit)
}

/// Resets the index and work tree from `old` to the tree `new`, throwing
/// away local changes to tracked files but refusing to overwrite
/// untracked ones (`--abort`, `--skip`); `fatal` is the message then.
fn reset_protecting_untracked(
    repo: &Repo,
    old: ObjectId,
    new: ObjectId,
    fatal: String,
) -> Result<(), Die> {
    let (index, _) = repo.read_index()?;
    let tracked: HashSet<&[u8]> = index.entries().iter().map(|e| e.path()).collect();
    let mut flat = crate::tree_changes::Flat::new();
    crate::tree_changes::flatten_tree(repo, &new, &mut flat)?;
    let work_tree = repo.work_tree()?;
    let mut in_the_way = Vec::new();
    for path in flat.keys() {
        if tracked.contains(path.as_slice()) {
            continue;
        }
        let p = work_tree.join(lossy(path));
        if std::fs::symlink_metadata(&p).is_ok_and(|m| !m.is_dir()) {
            in_the_way.push(path.clone());
        }
    }
    drop(index);
    if !in_the_way.is_empty() {
        let mut text = format!("error: {}\n", msg::UNTRACKED_OVERWRITTEN_BY_RESET);
        for p in &in_the_way {
            let mut q = Vec::new();
            crate::quote::write_quoted(&mut q, p, repo.quote_path());
            text.push('\t');
            text.push_str(&lossy(&q));
            text.push('\n');
        }
        text.push_str(msg::MOVE_OR_REMOVE_BEFORE_RESET);
        eprintln!("{text}");
        eprintln!("{}", rgit_unpack::messages::ABORTING);
        return Err(Die::Fatal(fatal));
    }
    crate::switch_branch::move_index(repo, old, new, true)?;
    Ok(())
}

/// `git rebase --continue`.
pub fn continue_rebase(ctx: &mut Context) -> CmdResult {
    let mut rb = Rebase::load(ctx)?;
    if rb.apply {
        let state = rb.state.clone();
        return apply::resume(rb.ctx, &state, "--continue");
    }
    let mut repo = rb.repo()?;
    let dir = state_dir(&repo);

    // The edited todo list was rejected: check it again and start.
    if !dir.join("msgnum").exists() {
        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let text = std::fs::read_to_string(dir.join("git-rebase-todo")).unwrap_or_default();
        {
            let mut resolve = |name: &str| resolve_named(&mut repo, name);
            if let Ok(t) = todo::parse(&text, &comment, &mut resolve) {
                write(&dir.join("end"), format!("{}\n", t.count()))?;
            }
        }
        let todo = match validate(
            rb.ctx,
            &mut repo,
            &text,
            &comment,
            Some(msg::PLEASE_FIX_WITH_EDIT_TODO),
        ) {
            Ok(t) => t,
            Err(()) => return Ok(1),
        };
        write(&dir.join("git-rebase-todo"), todo.text())?;
        update_refs::sync(&repo, &todo)?;
        drop(repo);
        return rb.run(todo);
    }

    let (index, mtime) = repo.read_index()?;
    let mut unmerged: Vec<Vec<u8>> = Vec::new();
    for e in index.entries() {
        if e.stage().is_conflict() && unmerged.last().map(Vec::as_slice) != Some(e.path()) {
            unmerged.push(e.path().to_vec());
        }
    }
    let checker = repo.checker(mtime)?;
    let dirty = index.entries().iter().any(|e| {
        !e.stage().is_conflict()
            && !e.mode.is_gitlink()
            && !matches!(
                checker.check(e),
                Ok(rgit_worktree::stat::Status::Unchanged | rgit_worktree::stat::Status::StatOnly)
            )
    });
    if !unmerged.is_empty() || dirty {
        for p in &unmerged {
            let mut q = Vec::new();
            crate::quote::write_quoted(&mut q, p, repo.quote_path());
            println!("{}", msg::needs_merge(&lossy(&q)));
        }
        println!("{}", msg::MUST_EDIT_CONFLICTS);
        return Ok(1);
    }
    let head = sequencer::head_commit(&mut repo)
        .ok_or_else(|| Die::Fatal(seqmsg::could_not_read("HEAD")))?;
    let head_tree = sequencer::tree_of(&repo, &head).map_err(fail_die)?;
    let staged = !sequencer::index_matches_tree(&repo, &index, &head_tree)?;
    drop(index);
    let stopped = std::fs::read_to_string(dir.join("stopped-sha"))
        .ok()
        .and_then(|s| parse_oid(&repo, &s));
    let amend = std::fs::read_to_string(dir.join("amend"))
        .ok()
        .and_then(|s| parse_oid(&repo, &s));
    let in_chain = dir.join(CURRENT_FIXUPS).exists();
    let next = match rb.read_todo()? {
        Ok(t) => t,
        Err(()) => return Ok(1),
    };
    let next_item = next.peek().cloned();
    let chain_ends = !next_item.as_ref().is_some_and(|i| i.cmd.melds());
    let continue_env = || {
        vec![(
            "GIT_REFLOG_ACTION".to_owned(),
            format!("{} (continue)", reflog_action()),
        )]
    };
    if staged && !in_chain {
        if amend.is_some_and(|a| a != head) {
            eprintln!("error: {}", msg::UNCOMMITTED_AFTER_EDIT);
            return Ok(1);
        }
        if amend.is_none() && stopped.is_none() {
            eprintln!("error: {}", msg::STAGED_CHANGES);
            return Ok(1);
        }
    }
    if staged {
        if in_chain {
            // A conflicted `squash`/`fixup`: the user edits the message.
            let comment = crate::commands::commit::comment_string(&repo, b"")?;
            let (template, _) = rb.squash_message(&repo, &comment).map_err(fail_die)?;
            write(&dir.join(MESSAGE_SQUASH), &template)?;
            let args = vec![
                "--amend".to_owned(),
                "--allow-empty".to_owned(),
                "-n".to_owned(),
                "-e".to_owned(),
                "-F".to_owned(),
                dir.join(MESSAGE_SQUASH).to_string_lossy().into_owned(),
            ];
            drop(repo);
            let code = rb.commit_child(&args, &continue_env(), false)?;
            if code != 0 {
                return Ok(code);
            }
        } else if amend.is_some() {
            // `edit`: the changes amend the stopped commit.
            drop(repo);
            let args = ["--amend", "-n", "-e"].map(str::to_owned);
            let code = rb.commit_child(&args, &continue_env(), false)?;
            if code != 0 {
                return Ok(code);
            }
        } else {
            if let Err(e) = check_author_script(&dir.join("author-script")) {
                eprintln!("error: {e}");
                eprintln!("error: {}", msg::STAGED_CHANGES);
                eprintln!("error: {}", msg::COULD_NOT_COMMIT_STAGED);
                return Ok(1);
            }
            let mut env: Vec<(String, String)> = read_author_script(&dir.join("author-script"));
            if rb.state.ignore_date {
                env.retain(|(k, _)| k != "GIT_AUTHOR_DATE");
                env.push(("GIT_AUTHOR_DATE".to_owned(), format!("@{}", now())));
            }
            if rb.state.cdate_is_adate
                && let Some((_, d)) = env.iter().find(|(k, _)| k == "GIT_AUTHOR_DATE")
            {
                env.push(("GIT_COMMITTER_DATE".to_owned(), d.clone()));
            }
            env.extend(continue_env());
            let message = dir.join("message");
            // git lets the user edit the message (when an editor is set up).
            let mut args = vec!["-n".to_owned(), "-e".to_owned()];
            if message.exists() {
                args.push("-F".to_owned());
                args.push(message.to_string_lossy().into_owned());
            }
            drop(repo);
            let code = rb.commit_child(&args, &env, false)?;
            if code != 0 {
                return Ok(code);
            }
        }
        let mut repo = rb.repo()?;
        if let (Some(old), Some(new)) = (stopped, sequencer::head_commit(&mut repo)) {
            rb.record_rewritten(&old, &new, next_item.as_ref())?;
        }
        if in_chain && chain_ends {
            rb.end_chain(&repo)?;
        } else if in_chain && let Some(h) = sequencer::head_commit(&mut repo) {
            // The edited message starts the rest of the chain.
            let commit = sequencer::read_commit(&repo, &h).map_err(fail_die)?;
            write(&dir.join(SQUASH_FIRST), commit.message())?;
            let _ = std::fs::remove_file(dir.join(CURRENT_FIXUPS));
        }
    } else if in_chain {
        rb.clear_stop()?;
        if let Some(code) = rb.drop_from_chain(&next, "continue")? {
            return Ok(code);
        }
    } else if amend.is_some()
        && let Some(old) = stopped
    {
        let mut repo = rb.repo()?;
        if let Some(new) = sequencer::head_commit(&mut repo) {
            rb.record_rewritten(&old, &new, next_item.as_ref())?;
        }
    }
    rb.clear_stop()?;
    rb.run(next)
}

/// `git rebase --skip`.
pub fn skip(ctx: &mut Context) -> CmdResult {
    let mut rb = Rebase::load(ctx)?;
    if rb.apply {
        let state = rb.state.clone();
        return apply::resume(rb.ctx, &state, "--skip");
    }
    let mut repo = rb.repo()?;
    if let Some(h) = sequencer::head_commit(&mut repo) {
        let tree = sequencer::tree_of(&repo, &h).map_err(fail_die)?;
        reset_protecting_untracked(&repo, tree, tree, msg::COULD_NOT_DISCARD.to_owned())?;
    }
    drop(repo);
    rb.clear_stop()?;
    let todo = match rb.read_todo()? {
        Ok(t) => t,
        Err(()) => return Ok(1),
    };
    if let Some(code) = rb.drop_from_chain(&todo, "skip")? {
        return Ok(code);
    }
    rb.run(todo)
}

/// `git rebase --abort`.
pub fn abort(ctx: &mut Context) -> CmdResult {
    let rb = Rebase::load(ctx)?;
    let dir = rb.dir()?;
    let mut repo = rb.repo()?;
    let head = sequencer::head_commit(&mut repo);
    let head_tree = match head {
        Some(h) => sequencer::tree_of(&repo, &h).map_err(fail_die)?,
        None => ObjectId::empty_tree(repo.hash),
    };
    let orig = rb.state.orig_head;
    let orig_tree = sequencer::tree_of(&repo, &orig).map_err(fail_die)?;
    reset_protecting_untracked(
        &repo,
        head_tree,
        orig_tree,
        msg::could_not_move_back(&orig.to_hex()),
    )?;
    let head_name = rb.state.head_name.clone();
    if head_name.starts_with("refs/") {
        let what = msg::reflog(&rb.action, "abort", &msg::returning_to(&head_name));
        attach_head(rb.ctx, &repo, &head_name, &what)?;
    } else {
        let what = msg::reflog(&rb.action, "abort", &msg::returning_to(&orig.to_hex()));
        detach_head(rb.ctx, &repo, orig, &what)?;
    }
    rb.clear_stop()?;
    delete_labels(rb.ctx, &repo)?;
    let _ = std::fs::remove_file(git_dir(&repo).join("AUTO_MERGE"));
    let _ = std::fs::remove_dir_all(dir);
    drop(repo);
    crate::maintenance::run_auto(rb.ctx);
    if let Some(s) = rb.state.autostash.clone() {
        apply_autostash(rb.ctx, &s)?;
    }
    Ok(0)
}

/// `git rebase --quit`.
pub fn quit(ctx: &mut Context) -> CmdResult {
    let rb = Rebase::load(ctx)?;
    let dir = rb.dir()?;
    if let Some(s) = rb.state.autostash.clone() {
        store_autostash(rb.ctx, &s)?;
    }
    let repo = rb.repo()?;
    delete_labels(rb.ctx, &repo)?;
    let _ = std::fs::remove_dir_all(dir);
    Ok(0)
}

/// `git rebase --edit-todo`.
pub fn edit_todo(ctx: &mut Context) -> CmdResult {
    let rb = Rebase::load(ctx)?;
    if rb.apply {
        return Err(Die::Fatal(msg::EDIT_TODO_NOT_INTERACTIVE.to_owned()));
    }
    let mut repo = rb.repo()?;
    let dir = state_dir(&repo);
    let path = dir.join("git-rebase-todo");
    let comment = crate::commands::commit::comment_string(&repo, b"")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed = {
        let mut resolve = |name: &str| resolve_named(&mut repo, name);
        todo::parse(&text, &comment, &mut resolve)
    };
    let help = todo_help(&repo, &comment, None);
    let mut shown = match parsed {
        Ok(t) => {
            // Once the rebase runs, missing commits are those removed now.
            if dir.join("msgnum").exists() {
                write(&dir.join("git-rebase-todo.backup"), t.text() + &help)?;
            }
            render_for_edit(&repo, &t)
        }
        Err(bad) => {
            print_bad(rb.ctx, &repo, &bad)?;
            text.clone()
        }
    };
    shown.push_str(&help);
    write(&path, shown)?;
    if let Err(code) = sequence_edit(rb.ctx, &repo, &path) {
        return Ok(code);
    }
    let edited = std::fs::read_to_string(&path).map_err(io_die)?;
    match validate(rb.ctx, &mut repo, &edited, &comment, None) {
        Ok(t) => {
            write(&path, t.text())?;
            update_refs::sync(&repo, &t)?;
            Ok(0)
        }
        Err(()) => Ok(1),
    }
}

/// Keeps the autostash `hex` in the stash list (`--quit`).
pub(crate) fn store_autostash(ctx: &Context, hex: &str) -> Result<(), Die> {
    run_child(
        ctx,
        &["stash", "store", "-q", "-m", msg::AUTOSTASH_MESSAGE, hex],
        true,
    )?;
    eprint!("{}", msg::AUTOSTASH_KEPT);
    Ok(())
}
