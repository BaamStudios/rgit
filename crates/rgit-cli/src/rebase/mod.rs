// SPDX-License-Identifier: MIT
//! `git rebase` with the merge back end, without the interactive todo
//! editing (Documentation/git-rebase.adoc, config/rebase.adoc), part of
//! #154. Interactive rebase (#155) builds on the same state and todo list.
//!
//! - Choosing the commits: `<upstream>..<branch>` without merges, oldest
//!   first, dropping commits whose change is already upstream (patch
//!   equivalence, [`patch_id`]) unless `--reapply-cherry-picks`, and
//!   commits that start empty with `--no-keep-empty`; `--onto`,
//!   `--keep-base`, `--root`, `--fork-point` and the configured upstream;
//! - "up to date" and fast-forward detection, `--force-rebase`;
//! - replaying the commits like `cherry-pick` (the merge, the two-way
//!   update of the index and work tree and the conflict stages come from
//!   [`crate::sequencer`]), the original author, `--signoff`,
//!   `--committer-date-is-author-date`, `--reset-author-date`, `--empty`;
//! - the state in `.git/rebase-merge/` in git's layout (`head-name`,
//!   `onto`, `orig-head`, `git-rebase-todo`, `done`, `msgnum`, `end`,
//!   `message`, `author-script`, `stopped-sha`, `patch`, option files),
//!   `REBASE_HEAD`, `MERGE_MSG`, `AUTO_MERGE`, `ORIG_HEAD` and the
//!   reflog messages `rebase (start|pick|continue|finish|abort)`;
//! - `--continue`, `--skip`, `--abort`, `--quit`, `--autostash`, the
//!   `pre-rebase` and `post-rewrite` hooks.
//!
//! The behaviour was derived from the documentation and differential
//! tests against the `git` binary (`tests/rebase.rs`).
//!
//! [`start`], [`continue_rebase`], [`skip`], [`abort`] and [`quit`] are the
//! entry points for `git rebase` and `git pull --rebase`.

pub(crate) mod patch_id;

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
}

/// One line of the todo list.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub commit: ObjectId,
    /// The rest of the line after the commit (`# <subject>`).
    pub rest: String,
}

impl Item {
    fn line(&self) -> String {
        format!("pick {} {}", self.commit.to_hex(), self.rest)
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
/// reflog (or its current value) that is an ancestor of `head`.
fn fork_point(repo: &Repo, refname: &str, head: &ObjectId) -> Result<Option<ObjectId>, Die> {
    let mut candidates = Vec::new();
    if let Some(cur) = repo.refs.resolve(refname) {
        candidates.push(cur);
    }
    let log = repo.setup.common_dir.join("logs").join(refname);
    let text = std::fs::read(&log).unwrap_or_default();
    let mut entries: Vec<ObjectId> = Vec::new();
    for line in text.split(|&b| b == b'\n') {
        let mut fields = line.split(|&b| b == b' ');
        let _old = fields.next();
        if let Some(new) = fields.next()
            && let Ok(id) = ObjectId::from_hex(repo.hash, new)
            && !id.is_null()
        {
            entries.push(id);
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

/// `--autostash`: stashes the local changes and resets to `HEAD`;
/// returns the stash commit.
fn create_autostash(ctx: &mut Context) -> Result<Option<String>, Die> {
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
fn apply_autostash(ctx: &mut Context, hex: &str) -> Result<(), Die> {
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
    eprint!("{}", msg::AUTOSTASH_CONFLICTS);
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
                Some(id) => (full, id, Some(b.clone())),
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

    // Up to date?
    let branch_short = head_name.strip_prefix("refs/heads/").map(str::to_owned);
    if let Some(r) = restrict
        && !o.root
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
            if !o.force {
                if let Some(b) = &switch_to {
                    let current = sequencer::head_commit(&mut repo);
                    let old_tree = match current {
                        Some(c) => sequencer::tree_of(&repo, &c).map_err(fail_die)?,
                        None => ObjectId::empty_tree(repo.hash),
                    };
                    let new_tree = sequencer::tree_of(&repo, &orig_head).map_err(fail_die)?;
                    crate::switch_branch::move_index(&repo, old_tree, new_tree, false)?;
                    let what = format!("{}: {}", reflog_action(), msg::checkout(b));
                    if head_name.starts_with("refs/") {
                        attach_head(ctx, &repo, &head_name, &what)?;
                    } else {
                        detach_head(ctx, &repo, orig_head, &what)?;
                    }
                }
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
        if let Some(base) = bases.first() {
            if o.verbose {
                println!("{}", msg::changes_from(&base.to_hex(), &onto.to_hex()));
            }
            let (b, n) = (base.to_hex(), onto.to_hex());
            run_child(ctx, &["diff", "--stat", "--summary", &b, &n], false)?;
        }
    }

    // The todo list.
    let reapply = o.reapply_cherry_picks.unwrap_or(o.keep_base);
    let items = todo_list(
        ctx,
        &repo,
        restrict,
        upstream.or(root_onto),
        orig_head,
        reapply,
        !o.no_keep_empty,
        quiet,
    )?;

    let state = State {
        head_name,
        onto,
        orig_head,
        quiet,
        verbose: o.verbose,
        signoff: o.signoff,
        cdate_is_adate: o.committer_date_is_author_date,
        ignore_date: o.ignore_date,
        empty: o.empty.unwrap_or_default(),
        strategy: o.strategy.clone(),
        xopts: o.xopts.clone(),
        autostash: stash,
        squash_onto,
        force: o.force || o.signoff || o.committer_date_is_author_date || o.ignore_date,
    };
    write_orig_head(ctx, &repo, orig_head)?;
    write_state(&repo, &state, &items)?;

    // Check out the new base.
    let current = sequencer::head_commit(&mut repo);
    let old_tree = match current {
        Some(c) => sequencer::tree_of(&repo, &c).map_err(fail_die)?,
        None => ObjectId::empty_tree(repo.hash),
    };
    let onto_tree = sequencer::tree_of(&repo, &onto).map_err(fail_die)?;
    if let Err(e) = crate::switch_branch::move_index(&repo, old_tree, onto_tree, false) {
        let _ = std::fs::remove_dir_all(state_dir(&repo));
        if let Die::Exit(code) = e {
            eprintln!("error: {}", msg::COULD_NOT_DETACH);
            return Ok(code);
        }
        return Err(e);
    }
    let action = reflog_action();
    detach_head(
        ctx,
        &repo,
        onto,
        &msg::reflog(&action, "start", &msg::checkout(&onto_name)),
    )?;
    crate::hooks::post_checkout(&repo, Some(orig_head), Some(onto), true)?;
    drop(repo);
    let mut rb = Rebase { ctx, state, action };
    rb.run(items)
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
) -> Result<Vec<Item>, Die> {
    let mut args = vec!["--topo-order".to_owned(), "--no-merges".to_owned()];
    if let Some(r) = restrict {
        args.push(format!("^{}", r.to_hex()));
    }
    args.push(head.to_hex());
    let commits = walk(ctx, &args)?;

    // Commits whose change is already upstream.
    let mut skipped: HashSet<ObjectId> = HashSet::new();
    if !reapply
        && let Some(up) = upstream
        && !commits.is_empty()
    {
        let theirs = walk(
            ctx,
            &[
                "--no-merges".to_owned(),
                format!("^{}", head.to_hex()),
                up.to_hex(),
            ],
        )?;
        if !theirs.is_empty() {
            let mut ours: Vec<(ObjectId, Vec<u8>, Vec<_>)> = Vec::new();
            for c in &commits {
                if let Some(ch) = patch_id::changes(repo, c) {
                    ours.push((*c, patch_id::path_key(&ch), ch));
                }
            }
            let keys: HashSet<&[u8]> = ours.iter().map(|(_, k, _)| k.as_slice()).collect();
            let mut full: HashSet<Vec<u8>> = HashSet::new();
            for t in &theirs {
                let Some(ch) = patch_id::changes(repo, t) else {
                    continue;
                };
                if ch.is_empty() || !keys.contains(patch_id::path_key(&ch).as_slice()) {
                    continue;
                }
                full.insert(patch_id::patch_key(repo, &ch));
            }
            if !full.is_empty() {
                for (c, _, ch) in &ours {
                    if !ch.is_empty() && full.contains(&patch_id::patch_key(repo, ch)) {
                        skipped.insert(*c);
                    }
                }
            }
        }
    }

    let mut items = Vec::new();
    let mut warned = false;
    for c in commits {
        if skipped.contains(&c) {
            if !quiet {
                eprintln!(
                    "warning: {}",
                    msg::skipped_cherry_pick(&repo.abbrev(&c, repo.default_abbrev()))
                );
            }
            warned = true;
            continue;
        }
        let Some(Object::Commit(commit)) = repo.read_object(&c) else {
            continue;
        };
        if !keep_empty {
            let tree = commit.tree().ok();
            let parent_tree = match commit.parents().unwrap_or_default().first() {
                Some(p) => crate::switch_branch::commit_tree(repo, p),
                None => Some(ObjectId::empty_tree(repo.hash)),
            };
            if tree.is_some() && tree == parent_tree {
                continue;
            }
        }
        items.push(Item {
            commit: c,
            rest: format!(
                "# {}",
                lossy(&rgit_commit::message::subject(commit.message()))
            ),
        });
    }
    if warned {
        ctx.advisor(&repo.config)?
            .advise(Advice::SkippedCherryPicks, msg::SKIPPED_CHERRY_PICK_HINT)
            .map_err(Die::Fatal)?;
    }
    Ok(items)
}

fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<(), Die> {
    std::fs::write(path, data).map_err(io_die)
}

/// Creates `.git/rebase-merge` for a new rebase.
fn write_state(repo: &Repo, s: &State, items: &[Item]) -> Result<(), Die> {
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
    if let Some(sq) = &s.squash_onto {
        write(&f("squash-onto"), format!("{}\n", sq.to_hex()))?;
    }
    write(&f("no-reschedule-failed-exec"), "")?;
    let mut todo = String::new();
    for i in items {
        todo.push_str(&i.line());
        todo.push('\n');
    }
    write(&f("git-rebase-todo"), &todo)?;
    write(&f("done"), "")?;
    write(&f("msgnum"), "0\n")?;
    write(&f("end"), format!("{}\n", items.len()))?;
    Ok(())
}

/// Reads the state of the rebase in progress.
fn read_state(repo: &Repo) -> Result<State, Die> {
    let dir = state_dir(repo);
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
        force: signoff || cdate_is_adate || ignore_date,
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

/// Parses a todo list (`git-rebase-todo`).
fn parse_todo(repo: &mut Repo, text: &str) -> Result<Vec<Item>, Die> {
    let mut items = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let end = line.find([' ', '\t']).unwrap_or(line.len());
        let word = &line[..end];
        match word {
            "pick" | "p" => {}
            "reword" | "r" | "edit" | "e" | "squash" | "s" | "fixup" | "f" | "exec" | "x"
            | "break" | "b" | "drop" | "d" | "label" | "l" | "reset" | "t" | "merge" | "m"
            | "update-ref" | "u" | "noop" => {
                return Err(Die::Fatal(msg::not_supported(&format!(
                    "the todo command '{word}' (#155)"
                ))));
            }
            _ => {
                eprintln!("error: {}", seqmsg::invalid_command(word));
                eprintln!("error: {}", seqmsg::invalid_line(n + 1, raw));
                return Err(Die::Exit(1));
            }
        }
        let rest = line[end..].trim_start();
        let id_end = rest.find([' ', '\t']).unwrap_or(rest.len());
        let name = &rest[..id_end];
        let Some(commit) = resolve_commit(repo, name).filter(|_| !name.is_empty()) else {
            eprintln!("error: {}", seqmsg::could_not_parse(name));
            eprintln!("error: {}", seqmsg::invalid_line(n + 1, raw));
            return Err(Die::Exit(1));
        };
        items.push(Item {
            commit,
            rest: rest[id_end..].trim_start().to_owned(),
        });
    }
    Ok(items)
}

// ----------------------------------------------------------------------
// Replaying.

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    Stopped,
    /// The commit could not be applied at all (local changes in the way):
    /// it goes back to the front of the todo list.
    Rescheduled,
}

struct Rebase<'c> {
    ctx: &'c mut Context,
    state: State,
    action: String,
}

impl<'c> Rebase<'c> {
    fn load(ctx: &'c mut Context) -> Result<Rebase<'c>, Die> {
        let repo = Repo::open(ctx, Mode::Required)?;
        repo.work_tree()?;
        if !state_dir(&repo).is_dir() {
            return Err(Die::Fatal(msg::NO_REBASE_IN_PROGRESS.to_owned()));
        }
        let state = read_state(&repo)?;
        drop(repo);
        Ok(Rebase {
            ctx,
            state,
            action: reflog_action(),
        })
    }

    fn repo(&self) -> Result<Repo, Die> {
        Repo::open(self.ctx, Mode::Required)
    }

    fn dir(&self) -> Result<PathBuf, Die> {
        Ok(state_dir(&self.repo()?))
    }

    fn git_path(&self, name: &str) -> Result<PathBuf, Die> {
        Ok(git_dir(&self.repo()?).join(name))
    }

    fn remove_git_file(&self, name: &str) -> Result<(), Die> {
        let _ = std::fs::remove_file(self.git_path(name)?);
        Ok(())
    }

    /// The remaining todo list.
    fn read_todo(&self) -> Result<Vec<Item>, Die> {
        let mut repo = self.repo()?;
        let text =
            std::fs::read_to_string(state_dir(&repo).join("git-rebase-todo")).unwrap_or_default();
        parse_todo(&mut repo, &text)
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

    /// Replays `todo`, then finishes the rebase.
    fn run(&mut self, mut todo: Vec<Item>) -> CmdResult {
        let dir = self.dir()?;
        let mut done = self.count_done()?;
        let total = done + todo.len();
        while !todo.is_empty() {
            let item = todo.remove(0);
            let mut done_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("done"))
                .map_err(io_die)?;
            writeln!(done_file, "{}", item.line()).map_err(io_die)?;
            drop(done_file);
            let mut rest = String::new();
            for i in &todo {
                rest.push_str(&i.line());
                rest.push('\n');
            }
            write(&dir.join("git-rebase-todo"), rest)?;
            done += 1;
            write(&dir.join("msgnum"), format!("{done}\n"))?;
            write(&dir.join("end"), format!("{total}\n"))?;
            if !self.state.quiet {
                let end = if self.state.verbose { "\n" } else { "\r" };
                eprint!("{}{end}", msg::progress(done, total));
            }
            match self.pick(&item).map_err(fail_die)? {
                Step::Done => {}
                Step::Stopped => return Ok(1),
                Step::Rescheduled => {
                    todo.insert(0, item);
                    return self.reschedule(&todo);
                }
            }
        }
        self.finish()
    }

    /// Replays one commit onto `HEAD`.
    fn pick(&mut self, item: &Item) -> Result<Step, Fail> {
        let mut repo = self.repo()?;
        let id = item.commit;
        let head = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Fail::Error(seqmsg::could_not_read("HEAD")))?;
        let head_tree = sequencer::tree_of(&repo, &head)?;
        let commit = sequencer::read_commit(&repo, &id)?;
        let parent = commit.parents().unwrap_or_default().first().copied();
        let empty_tree = ObjectId::empty_tree(repo.hash);

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
            self.record_rewritten(&id, &id)?;
            return Ok(Step::Done);
        }

        let abbrev = repo.abbrev(&id, repo.default_abbrev());
        let subject = lossy(sequencer::subject_line(commit.message()));
        let label = format!("{abbrev} ({subject})");
        let commit_tree = commit.tree().map_err(|e| Fail::Error(e.to_string()))?;
        let parent_tree = match parent {
            Some(p) => sequencer::tree_of(&repo, &p)?,
            None => empty_tree,
        };
        let parent_label = match parent {
            Some(_) => seqmsg::parent_of(&label),
            None => seqmsg::EMPTY_TREE_LABEL.to_owned(),
        };
        let result = sequencer::merge_with(
            &repo,
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
        match sequencer::two_way(&repo, head_tree, result.tree, Some(&result), true) {
            Err(Fail::Printed) => return Ok(Step::Rescheduled),
            r => r?,
        }
        {
            let mut out = Vec::new();
            for m in &result.messages {
                out.extend_from_slice(&m.text);
                out.push(b'\n');
            }
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
        crate::commands::merge_tree::advise_submodules(self.ctx, &repo, &result)?;
        write(
            &git_dir(&repo).join("AUTO_MERGE"),
            format!("{}\n", result.tree.to_hex()),
        )?;

        let comment = crate::commands::commit::comment_string(&repo, b"")?;
        let mut message = commit.message().to_vec();
        if self.state.signoff {
            message = sequencer::signoff_message(&repo, &message, &comment)?;
        }
        let original_author = self.author_of(&commit, &id, false)?;
        let author = self.author_of(&commit, &id, self.state.ignore_date)?;

        if !result.clean {
            let paths: Vec<Vec<u8>> = result.conflicts.iter().map(|c| c.path.clone()).collect();
            let mut m = message.clone();
            m.extend_from_slice(&sequencer::conflicts_block(
                &comment,
                &paths,
                repo.quote_path(),
            ));
            self.write_stopped(&repo, &id, &m, &original_author, false)?;
            eprintln!("error: {}", seqmsg::could_not_apply(&abbrev, &subject));
            self.ctx
                .advisor(&repo.config)?
                .advise(Advice::MergeConflict, msg::CONFLICT_ADVICE)
                .map_err(Die::Fatal)?;
            eprintln!("{}", msg::could_not_apply(&abbrev, &item.rest));
            return Ok(Step::Stopped);
        }

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
                    eprintln!("{}", msg::could_not_apply(&abbrev, &item.rest));
                    return Ok(Step::Stopped);
                }
            }
        }

        let new = self.commit(
            &mut repo,
            head,
            result.tree,
            &commit,
            &author,
            message,
            &comment,
        )?;
        self.record_rewritten(&id, &new)?;
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

    /// Creates the replayed commit: hooks `prepare-commit-msg` and
    /// `post-commit`, the reflog entry `rebase (pick): <subject>`.
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &mut self,
        repo: &mut Repo,
        head: ObjectId,
        tree: ObjectId,
        picked: &Commit,
        author: &Signature,
        message: Vec<u8>,
        comment: &str,
    ) -> Result<ObjectId, Fail> {
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
        let parents = [head];
        let new = rgit_commit::NewCommit {
            tree,
            parents: &parents,
            author,
            committer: &committer,
            encoding: picked.encoding(),
            message: &message,
        };
        let oid = rgit_commit::create_commit(repo.odb(), &new)
            .map_err(|e| Fail::Die(Die::Fatal(e.to_string())))?;
        let subject = lossy(rgit_commit::message::first_line(&message));
        let what = msg::reflog(&self.action, "pick", &subject);
        sequencer::update_head(self.ctx, repo, oid, Some(head), &what)?;
        let o = RunOptions::with_args(std::iter::empty::<&str>());
        crate::hooks::run(&hooks, "post-commit", &o)?;
        Ok(oid)
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
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("commit")
            .args(args)
            .envs(self.ctx.child_env())
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
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

    /// Puts the commit that could not be applied back on the todo list.
    fn reschedule(&mut self, todo: &[Item]) -> CmdResult {
        let repo = self.repo()?;
        let dir = state_dir(&repo);
        let mut rest = String::new();
        for i in todo {
            rest.push_str(&i.line());
            rest.push('\n');
        }
        write(&dir.join("git-rebase-todo"), rest)?;
        let item = &todo[0];
        write(
            &git_dir(&repo).join("REBASE_HEAD"),
            format!("{}\n", item.commit.to_hex()),
        )?;
        if let Some(Object::Commit(c)) = repo.read_object(&item.commit)
            && let Some(a) = c.author().and_then(|a| a.to_owned())
        {
            write(&dir.join("author-script"), author_script(&a))?;
        }
        let advisor = self.ctx.advisor(&repo.config)?;
        eprint!("{}", advisor.hint(&msg::rescheduled(&item.line())));
        Ok(1)
    }

    fn record_rewritten(&self, old: &ObjectId, new: &ObjectId) -> Result<(), Die> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir()?.join("rewritten-list"))
            .map_err(io_die)?;
        writeln!(f, "{} {}", old.to_hex(), new.to_hex()).map_err(io_die)
    }

    /// Updates the branch, runs `post-rewrite`, removes the state and
    /// applies the autostash.
    fn finish(&mut self) -> CmdResult {
        let mut repo = self.repo()?;
        let head = sequencer::head_commit(&mut repo)
            .ok_or_else(|| Die::Fatal(seqmsg::could_not_read("HEAD")))?;
        let head_name = self.state.head_name.clone();
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
        let dir = state_dir(&repo);
        let rewritten = std::fs::read(dir.join("rewritten-list")).unwrap_or_default();
        if !rewritten.is_empty() {
            let hooks = crate::hooks::for_repo(self.ctx, &repo.setup, &repo.config)?;
            if hooks.exists("post-rewrite") {
                let mut o = RunOptions::with_args(["rebase"]);
                o.stdin = Input::Data(&rewritten);
                crate::hooks::run(&hooks, "post-rewrite", &o)?;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(git_dir(&repo).join("REBASE_HEAD"));
        drop(repo);
        if let Some(s) = self.state.autostash.clone() {
            apply_autostash(self.ctx, &s)?;
        }
        if !self.state.quiet {
            eprintln!("{}", msg::success(&head_name));
        }
        Ok(0)
    }

    /// Forgets the stop: the pseudo-refs and the stopped commit's files.
    fn clear_stop(&self) -> Result<(), Die> {
        for f in ["REBASE_HEAD", "MERGE_MSG", "CHERRY_PICK_HEAD"] {
            self.remove_git_file(f)?;
        }
        let dir = self.dir()?;
        for f in ["message", "author-script", "stopped-sha", "patch"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        Ok(())
    }
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
    let mut repo = rb.repo()?;
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
    let dir = state_dir(&repo);
    let stopped = std::fs::read_to_string(dir.join("stopped-sha"))
        .ok()
        .and_then(|s| parse_oid(&repo, &s));
    if staged {
        let _ = std::fs::remove_file(git_dir(&repo).join("CHERRY_PICK_HEAD"));
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
        env.push((
            "GIT_REFLOG_ACTION".to_owned(),
            format!("{} (continue)", rb.action),
        ));
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
        let mut repo = rb.repo()?;
        if let (Some(old), Some(new)) = (stopped, sequencer::head_commit(&mut repo)) {
            rb.record_rewritten(&old, &new)?;
        }
    }
    rb.clear_stop()?;
    let todo = rb.read_todo()?;
    rb.run(todo)
}

/// `git rebase --skip`.
pub fn skip(ctx: &mut Context) -> CmdResult {
    let mut rb = Rebase::load(ctx)?;
    let mut repo = rb.repo()?;
    if let Some(h) = sequencer::head_commit(&mut repo) {
        let tree = sequencer::tree_of(&repo, &h).map_err(fail_die)?;
        reset_protecting_untracked(&repo, tree, tree, msg::COULD_NOT_DISCARD.to_owned())?;
    }
    drop(repo);
    rb.clear_stop()?;
    let todo = rb.read_todo()?;
    rb.run(todo)
}

/// `git rebase --abort`.
pub fn abort(ctx: &mut Context) -> CmdResult {
    let rb = Rebase::load(ctx)?;
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
    let _ = std::fs::remove_file(git_dir(&repo).join("AUTO_MERGE"));
    let _ = std::fs::remove_dir_all(state_dir(&repo));
    drop(repo);
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
        run_child(
            rb.ctx,
            &["stash", "store", "-q", "-m", msg::AUTOSTASH_MESSAGE, &s],
            true,
        )?;
        eprint!("{}", msg::AUTOSTASH_KEPT);
    }
    let _ = std::fs::remove_dir_all(dir);
    Ok(0)
}
