// SPDX-License-Identifier: MIT
//! Moving `HEAD` to another branch or commit together with the index and
//! the work tree: the part `switch` and `checkout <branch>` share
//! (Documentation/git-switch.adoc, git-checkout.adoc; output, reflog
//! entries and the resulting index checked against the `git` binary).
//!
//! The index and work tree move with a two-way merge from the old `HEAD`
//! tree to the new one (`rgit-unpack`), which keeps local changes that do
//! not conflict and refuses with git's grouped messages otherwise; with
//! `--force`/`--discard-changes` a one-way merge with `reset` replaces
//! everything.

use std::io::Write as _;

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_index::{Entry, Index, Stage, WriteOptions};
use rgit_object::ObjectKind;
use rgit_refs::files::FilesStore;
use rgit_refs::{RefStore, RefUpdate, Target as RefTarget, Transaction};
use rgit_revwalk::walk::{CommitInfo, CommitSource, Walk, WalkOptions};
use rgit_unpack::{Identity, Merge, Options, Porcelain, Worktree};
use rgit_worktree::stat::{Checker, Status, smudge_racy_entries};

use crate::commands::read_tree::{lock_failed, path_options, unpack_failed};
use crate::messages::switch as msg;
use crate::messages::wt_status as wmsg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repository::Repo;
use crate::tracking;

/// Where `HEAD` goes.
pub enum Target {
    /// A local branch (short name).
    Branch(String),
    /// A detached commit; `name` is how the user named it (for the reflog
    /// and the advice).
    Detached { name: String },
}

/// A branch created (or reset) on the way.
pub struct Create {
    /// Short name.
    pub name: String,
    /// The start point as given (`HEAD` when omitted), for the reflog.
    pub start: String,
    /// `-B`/`-C`: an existing branch is reset.
    pub reset: bool,
    /// Upstream tracking, from [`tracking::plan`].
    pub track: Option<(tracking::Plan, tracking::AutoSetup)>,
    /// A start point was given; without one the index and work tree are
    /// left alone.
    pub start_given: bool,
    /// `-l`: create the branch's reflog even when `core.logAllRefUpdates`
    /// would not.
    pub reflog: bool,
}

/// What to do.
pub struct Request {
    pub target: Target,
    /// The commit to move to.
    pub commit: ObjectId,
    pub create: Option<Create>,
    /// `-f`, `--discard-changes`.
    pub force: bool,
    pub quiet: bool,
    /// `checkout` (not `switch`, not `--detach`): advise when detaching.
    pub advise_detach: bool,
    /// `switch`: refuse while a merge, rebase, am, cherry-pick or revert
    /// is in progress (warn while bisecting).
    pub refuse_in_progress: bool,
}

/// `switch` refuses to leave a merge, rebase, am session, cherry-pick or
/// revert in progress (git-switch.adoc); bisecting only gets a warning.
pub fn check_in_progress(repo: &Repo) -> Result<(), Die> {
    let dir = &repo.setup.git_dir;
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let pseudo = |name: &str| store.resolve_oid(name).ok().flatten().is_some();
    let refuse =
        |what: &str, command: &str| Err(Die::Fatal(msg::cannot_switch_in_progress(what, command)));
    if dir.join("MERGE_HEAD").is_file() {
        return refuse("while merging", "merge");
    }
    if dir.join("rebase-apply").is_dir() {
        if dir.join("rebase-apply/applying").exists() {
            return refuse("in the middle of an am session", "am");
        }
        return refuse("while rebasing", "rebase");
    }
    if dir.join("rebase-merge").is_dir() {
        return refuse("while rebasing", "rebase");
    }
    if pseudo("CHERRY_PICK_HEAD") {
        return refuse("while cherry-picking", "cherry-pick");
    }
    if pseudo("REVERT_HEAD") {
        return refuse("while reverting", "revert");
    }
    if dir.join("BISECT_LOG").is_file() {
        eprintln!("warning: {}", msg::SWITCHING_WHILE_BISECTING);
    }
    Ok(())
}

/// The `tree` of commit `id`.
pub fn commit_tree(repo: &Repo, id: &ObjectId) -> Option<ObjectId> {
    match repo.read_object(id)? {
        rgit_object::Object::Commit(c) => c.tree().ok(),
        _ => None,
    }
}

/// The subject of commit `id`: the first paragraph of its message, lines
/// joined with spaces.
pub fn subject(objects: &Objects, id: &ObjectId) -> String {
    let Some((_, data)) = objects.read(id) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&data);
    let body = text.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    body.lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Records fresh stat data for entries whose files were only touched, so
/// the merge does not take them for local changes (porcelain refreshes
/// before merging).
pub fn refresh_stat(index: &mut Index, checker: &Checker) {
    for pos in 0..index.len() {
        let e = &index.entries()[pos];
        if e.stage() != Stage::MERGED
            || e.flags.skip_worktree
            || e.flags.assume_valid
            || e.flags.intent_to_add
            || e.mode.is_gitlink()
        {
            continue;
        }
        if let Ok(Status::StatOnly) = checker.check(e) {
            if let Ok(Some(meta)) = checker.lstat(e.path()) {
                if let Some(entry) = index.entry_mut(pos) {
                    checker.refresh(entry, &meta);
                }
            }
        }
    }
}

/// Writes `index` into the held `index.lock` and commits it.
pub fn write_index(repo: &Repo, index: &Index, mut lock: LockFile) -> Result<(), Die> {
    let skip_hash = repo
        .config
        .bool("index.skiphash")
        .ok()
        .flatten()
        .unwrap_or(false);
    let fail = || Die::Fatal(crate::messages::read_tree::UNABLE_TO_WRITE_INDEX.into());
    index
        .write_locked(&mut lock, &WriteOptions { skip_hash })
        .map_err(|_| fail())?;
    lock.commit().map_err(|_| fail())
}

/// The outcome of [`move_index`].
pub struct Moved {
    /// `<letter>\t<path>` lines of local changes carried over (diff of the
    /// new tree against the index and work tree).
    pub changes: Vec<u8>,
}

/// Moves the index and work tree from `old` to `new` (trees); with
/// `force` a one-way reset to `new`. Prints git's refusal and returns
/// `Die::Exit(1)` when local work would be lost.
pub fn move_index(repo: &Repo, old: ObjectId, new: ObjectId, force: bool) -> Result<Moved, Die> {
    move_index_with(repo, old, new, force, Porcelain::Checkout)
}

/// [`move_index`] with the wording of `cmd`.
pub fn move_index_with(
    repo: &Repo,
    old: ObjectId,
    new: ObjectId,
    force: bool,
    cmd: Porcelain,
) -> Result<Moved, Die> {
    let work_tree = repo.work_tree()?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
    let (mut index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    refresh_stat(&mut index, &checker);
    let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
    let wt = Worktree {
        root: &work_tree,
        checker: &checker,
    };
    let merge = if force {
        Merge::OneWay(new)
    } else {
        Merge::TwoWay { old, new }
    };
    let mut opts = Options {
        reset: force,
        update: true,
        show_all_errors: true,
        initial_checkout: index.is_empty() && mtime.is_none(),
        paths: path_options(repo),
        excludes: Some(&mut excludes),
        ..Options::default()
    };
    let plan = match rgit_unpack::plan(repo.odb(), &index, &merge, Some(&wt), &mut opts) {
        Ok(p) => p,
        Err(rgit_unpack::Error::Rejected(r)) => {
            for m in r.porcelain_messages(cmd) {
                eprintln!("error: {m}");
            }
            eprintln!("{}", rgit_unpack::messages::ABORTING);
            return Err(Die::Exit(1));
        }
        Err(e) => return Err(unpack_failed(e)),
    };
    let wrote_files = !plan.updates().is_empty();
    let applied = plan.apply(repo.odb(), &wt, &Identity);
    if wrote_files && crate::commands::add::attributes_may_apply(repo, &work_tree) {
        eprintln!(
            "warning: {}",
            crate::messages::read_tree::ATTRIBUTES_NOT_APPLIED
        );
    }
    let mut index = applied.index;
    if let Some(reference) = mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    let changes = if force {
        Vec::new()
    } else {
        local_changes(repo, &index, new, &checker)?
    };
    write_index(repo, &index, lock)?;
    if !applied.errors.is_empty() {
        for e in &applied.errors {
            eprintln!("error: {e}");
        }
        return Err(Die::Exit(128));
    }
    Ok(Moved { changes })
}

/// `git diff-index --name-status <tree>` of the index and work tree: the
/// local changes `checkout` lists after switching.
fn local_changes(
    repo: &Repo,
    index: &Index,
    tree: ObjectId,
    checker: &Checker,
) -> Result<Vec<u8>, Die> {
    let tree = rgit_unpack::tree_entries(repo.odb(), &tree).map_err(unpack_failed)?;
    let quote = repo.quote_path();
    let mut out = Vec::new();
    let mut line = |letter: u8, path: &[u8]| {
        out.push(letter);
        out.push(b'\t');
        crate::quote::write_quoted(&mut out, path, quote);
        out.push(b'\n');
    };
    let groups: Vec<&[Entry]> = index
        .entries()
        .chunk_by(|a, b| a.path() == b.path())
        .collect();
    let (mut gi, mut ti) = (0, 0);
    loop {
        let g = groups.get(gi);
        let t = tree.get(ti);
        let order = match (g, t) {
            (None, None) => break,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(g), Some(t)) => g[0].path().cmp(t.path()),
        };
        match order {
            std::cmp::Ordering::Greater => {
                line(b'D', t.expect("tree entry").path());
                ti += 1;
            }
            std::cmp::Ordering::Less => {
                let g = g.expect("index entry");
                gi += 1;
                if g.iter().any(|e| e.stage().is_conflict()) {
                    line(b'U', g[0].path());
                } else if checker.lstat(g[0].path()).ok().flatten().is_some() {
                    line(b'A', g[0].path());
                }
            }
            std::cmp::Ordering::Equal => {
                let g = g.expect("index entry");
                let t = t.expect("tree entry");
                gi += 1;
                ti += 1;
                if g.iter().any(|e| e.stage().is_conflict()) {
                    line(b'U', g[0].path());
                    continue;
                }
                let e = &g[0];
                if e.mode.is_gitlink() {
                    if e.id != t.id {
                        line(b'M', e.path());
                    }
                    continue;
                }
                let st = checker
                    .check(e)
                    .map_err(|err| Die::Fatal(err.to_string()))?;
                match st {
                    Status::Deleted => line(b'D', e.path()),
                    Status::TypeChanged => line(b'T', e.path()),
                    _ if (e.mode.0 & 0o170000) != (t.mode.0 & 0o170000) => line(b'T', e.path()),
                    Status::Modified => line(b'M', e.path()),
                    _ if e.mode != t.mode || e.id != t.id || e.flags.intent_to_add => {
                        line(b'M', e.path())
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// Carries out `req`: index and work tree, branch creation, `HEAD` and
/// its reflog, and the messages.
pub fn switch(repo: &Repo, revs: &Revs, req: Request) -> CmdResult {
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let objects = Objects::open(&repo.setup);
    let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
    let hash = repo.hash;
    if req.refuse_in_progress {
        check_in_progress(repo)?;
    }

    let head_raw = store.read_raw("HEAD").ok().flatten();
    let old_branch: Option<String> = match &head_raw {
        Some(RefTarget::Symbolic(r)) => Some(r.clone()),
        _ => None,
    };
    let old_oid = store.resolve_oid("HEAD").ok().flatten();
    let new_oid = req.commit;

    let empty = ObjectId::empty_tree(hash);
    let old_tree = match old_oid {
        Some(id) => commit_tree(repo, &id).unwrap_or(empty),
        None => empty,
    };
    let new_tree = commit_tree(repo, &new_oid)
        .ok_or_else(|| Die::Fatal(msg::unable_to_read_tree(&new_oid.to_hex())))?;

    // `checkout -b <new>` without a start point leaves the index alone.
    let skip_merge = !req.force
        && req.create.as_ref().is_some_and(|c| !c.start_given)
        && old_oid == Some(new_oid);
    let moved = if skip_merge {
        None
    } else {
        Some(move_index(repo, old_tree, new_tree, req.force)?)
    };
    if let Some(m) = &moved {
        if !req.quiet && !m.changes.is_empty() {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&m.changes);
            let _ = o.flush();
        }
    }

    let run = |u: RefUpdate, hooks: &mut UpdateHooks<'_>| -> Result<(), Die> {
        let mut tx = Transaction::new(&store);
        tx.push(u)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| Die::Fatal(e.to_string()))
    };

    let mut existed = false;
    if let Some(c) = &req.create {
        let full = format!("refs/heads/{}", c.name);
        let old = store.resolve_oid(&full).ok().flatten();
        existed = old.is_some();
        let reason = if existed {
            msg::reset_to(&c.start)
        } else {
            msg::created_from(&c.start)
        };
        let update = match old {
            Some(o) => RefUpdate::update(full.as_str(), new_oid, Some(o)),
            None => RefUpdate::update(full.as_str(), new_oid, Some(ObjectId::null(hash))),
        };
        run(
            update.message(reason.as_str()).create_reflog(c.reflog),
            &mut hooks,
        )?;
        if let Some((plan, auto)) = &c.track {
            let trepo = tracking::Repo {
                setup: &repo.setup,
                config: &repo.config,
                store: &store,
                revs,
            };
            plan.apply(&trepo, &c.name, auto, req.quiet)?;
        }
    }

    // Like git, leaving for another commit or branch ends a merge,
    // cherry-pick or revert in progress.
    let cancelled = crate::commands::reset::remove_branch_state(&repo.setup.git_dir, hash);

    let from = match &old_branch {
        Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_owned(),
        None => old_oid.map(|o| o.to_hex()).unwrap_or_default(),
    };
    let say = |s: &str| {
        if !req.quiet {
            eprintln!("{s}");
        }
    };
    let old_detached = old_branch.is_none();
    let abbrev_len = repo.default_abbrev();
    let previous = || -> Result<(), Die> {
        match old_oid {
            Some(o) if old_detached && o != new_oid && !req.quiet => {
                report_old_head(repo, &store, &objects, o, Some(new_oid))
            }
            _ => Ok(()),
        }
    };

    match &req.target {
        Target::Detached { name } => {
            let reason = msg::reflog_moving(&from, name);
            run(
                RefUpdate::update("HEAD", new_oid, None)
                    .no_deref(true)
                    .message(reason.as_str()),
                &mut hooks,
            )?;
            previous()?;
            let advice = advisor(repo)?
                .enabled(Advice::DetachedHead)
                .map_err(Die::Fatal)?;
            if req.advise_detach && !old_detached && advice && !req.quiet {
                eprintln!("{}", msg::detached_advice(name));
            }
            say(&msg::head_now_at(
                &head_abbrev(&objects, &new_oid, abbrev_len),
                &subject(&objects, &new_oid),
            ));
        }
        Target::Branch(name) => {
            let full = format!("refs/heads/{name}");
            let reason = msg::reflog_moving(&from, name);
            hooks.mark_symbolic("HEAD");
            run(
                RefUpdate::symref_update("HEAD", full.as_str(), None)
                    .no_deref(true)
                    .message(reason.as_str()),
                &mut hooks,
            )?;
            previous()?;
            let same = old_branch.as_deref() == Some(full.as_str());
            match &req.create {
                Some(c) if c.reset && existed => {
                    if same {
                        say(&msg::reset_branch(name));
                    } else {
                        say(&msg::switched_and_reset(name));
                    }
                }
                Some(_) => say(&msg::switched_new(name)),
                None if same => say(&msg::already_on(name)),
                None => say(&msg::switched(name)),
            }
            if !req.quiet {
                print_tracking(repo, name, new_oid);
            }
        }
    }
    warn_cancelled(cancelled, req.quiet);
    Ok(0)
}

/// "warning: cancelling a ... in progress" after [`remove_branch_state`]
/// ended a cherry-pick or revert.
///
/// [`remove_branch_state`]: crate::commands::reset::remove_branch_state
pub fn warn_cancelled(cancelled: Option<&str>, quiet: bool) {
    if let Some(what) = cancelled.filter(|_| !quiet) {
        eprintln!("warning: {}", msg::cancelling(what));
    }
}

/// `id` abbreviated for "HEAD is now at" and "Previous HEAD position
/// was", followed by "..." when `GIT_PRINT_SHA1_ELLIPSIS` is `yes` (any
/// case).
fn head_abbrev(objects: &Objects, id: &ObjectId, len: usize) -> String {
    let mut s = objects.abbrev(id, len);
    if std::env::var("GIT_PRINT_SHA1_ELLIPSIS").is_ok_and(|v| v.eq_ignore_ascii_case("yes")) {
        s.push_str("...");
    }
    s
}

/// Leaving the detached `HEAD` at `old` for `new`: "Previous HEAD position
/// was ...", or the warning about commits no ref reaches any more (with
/// the `detachedHead` advice on how to keep them).
pub fn report_old_head(
    repo: &Repo,
    store: &FilesStore,
    objects: &Objects,
    old: ObjectId,
    new: Option<ObjectId>,
) -> Result<(), Die> {
    let abbrev_len = repo.default_abbrev();
    let lost = lost_commits(store, objects, old, new);
    if lost.is_empty() {
        eprintln!(
            "{}",
            msg::previous_head(
                &head_abbrev(objects, &old, abbrev_len),
                &subject(objects, &old)
            )
        );
        return Ok(());
    }
    let mut out = msg::leaving_behind(lost.len());
    out.push('\n');
    // Up to five commits; more than that shows four and a count.
    let shown = if lost.len() > 5 { 4 } else { lost.len() };
    for id in &lost[..shown] {
        out.push_str(&msg::leaving_behind_commit(
            &objects.abbrev(id, abbrev_len),
            &subject(objects, id),
        ));
        out.push('\n');
    }
    if shown < lost.len() {
        out.push_str(&msg::leaving_behind_more(lost.len() - shown));
        out.push('\n');
    }
    out.push('\n');
    if advisor(repo)?
        .enabled(Advice::DetachedHead)
        .map_err(Die::Fatal)?
    {
        out.push_str(&msg::leaving_behind_advice(
            lost.len(),
            &objects.abbrev(&old, abbrev_len),
        ));
        out.push('\n');
    }
    eprint!("{out}");
    Ok(())
}

/// Commits reachable from `old` but neither from `new` (if any) nor from
/// any ref,
/// newest first.
fn lost_commits(
    store: &FilesStore,
    objects: &Objects,
    old: ObjectId,
    new: Option<ObjectId>,
) -> Vec<ObjectId> {
    let mut walk = Walk::new(CommitReader(objects), WalkOptions::default());
    if walk.push(&old, false).is_err() {
        return Vec::new();
    }
    if let Some(new) = new {
        let _ = walk.push(&new, true);
    }
    let refs = store
        .iter(&rgit_refs::IterOptions::prefix(""))
        .unwrap_or_default();
    for item in refs {
        if let rgit_refs::IterItem::Ref(r) = item {
            if let Some(c) = r.oid.and_then(|id| peel_commit(objects, &id)) {
                let _ = walk.push(&c, true);
            }
        }
    }
    walk.collect_all().unwrap_or_default()
}

/// Commits for the walk, read from the object database.
struct CommitReader<'a>(&'a Objects);

impl CommitSource for CommitReader<'_> {
    type Error = ();
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ()> {
        let (kind, data) = self.0.read(id).ok_or(())?;
        if kind != ObjectKind::Commit {
            return Err(());
        }
        let commit = rgit_object::Commit::parse(self.0.hash(), &data).map_err(|_| ())?;
        Ok(CommitInfo {
            parents: commit.parents().map_err(|_| ())?,
            time: commit
                .committer()
                .and_then(|s| s.time())
                .map_or(0, |t| t.seconds),
        })
    }
}

/// "Your branch is ..." for `branch` at `head`, on stdout.
pub fn print_tracking(repo: &Repo, branch: &str, head: ObjectId) {
    let Some(t) = crate::wt_status::tracking(repo, branch, Some(head)) else {
        return;
    };
    let hints = advisor(repo)
        .ok()
        .and_then(|a| a.enabled(Advice::StatusHints).ok())
        .unwrap_or(true);
    let mut lines: Vec<String> = Vec::new();
    match t.counts {
        None => {
            lines.push(wmsg::gone(&t.name));
            if hints {
                lines.push(wmsg::HINT_UNSET_UPSTREAM.into());
            }
        }
        Some((0, 0)) => lines.push(wmsg::up_to_date(&t.name)),
        Some((a, 0)) => {
            lines.push(wmsg::ahead(&t.name, a));
            if hints {
                lines.push(wmsg::HINT_PUSH.into());
            }
        }
        Some((0, b)) => {
            lines.push(wmsg::behind(&t.name, b));
            if hints {
                lines.push(wmsg::HINT_PULL_FF.into());
            }
        }
        Some((a, b)) => {
            lines.extend(wmsg::diverged(&t.name, a, b));
            if hints {
                lines.push(msg::HINT_PULL_DIVERGED.into());
            }
        }
    }
    let mut o = std::io::stdout().lock();
    for l in lines {
        let _ = writeln!(o, "{l}");
    }
    let _ = o.flush();
}

/// `-` / `@{-1}`: the branch (or commit name) checked out before.
pub fn previous_checkout(repo: &Repo, n: usize) -> Option<String> {
    let logs = rgit_refs::reflog::FilesReflog::new(
        repo.setup.git_dir.clone(),
        repo.setup.common_dir.clone(),
        repo.hash,
    );
    let head = logs.read("HEAD").ok().flatten()?;
    rgit_refs::reflog::nth_prior_checkout(&head, n).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// Validates a new branch name like git (`fatal:` plus the ref-syntax
/// hint).
pub fn check_new_branch_name(repo: &Repo, name: &str) -> Result<(), Die> {
    let ok = name != "HEAD"
        && !name.starts_with('-')
        && rgit_refs::name::check_branch_name(name.as_bytes()).is_ok();
    if ok {
        return Ok(());
    }
    eprintln!("fatal: {}", msg::not_valid_branch_name(name));
    advisor(repo)?
        .advise(Advice::RefSyntax, crate::messages::branch::HINT_REF_SYNTAX)
        .map_err(Die::Fatal)?;
    Err(Die::Exit(128))
}

/// The advice settings of `repo` (`--no-advice` is known process-wide).
fn advisor(repo: &Repo) -> Result<rgit_messages::Advisor, Die> {
    rgit_messages::Advisor::new(&repo.config, &rgit_config::SystemEnv::default(), false)
        .map_err(Die::Fatal)
}

/// `--orphan <name>`: `HEAD` becomes the unborn branch `name`; with
/// `clear` (`switch`) the tracked files are removed first. `reflog`
/// (`-l`): create the branch's (empty) reflog now.
pub fn orphan(repo: &Repo, name: &str, clear: bool, quiet: bool, reflog: bool) -> CmdResult {
    check_new_branch_name(repo, name)?;
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let full = format!("refs/heads/{name}");
    if store.resolve_oid(&full).ok().flatten().is_some() {
        return Err(Die::Fatal(msg::branch_exists(name)));
    }
    if clear {
        check_in_progress(repo)?;
    }
    let detached_at = match store.read_raw("HEAD") {
        Ok(Some(RefTarget::Object(id))) => Some(id),
        _ => None,
    };
    if clear {
        let old = store.resolve_oid("HEAD").ok().flatten();
        let empty = ObjectId::empty_tree(repo.hash);
        let old_tree = old.and_then(|o| commit_tree(repo, &o)).unwrap_or(empty);
        move_index(repo, old_tree, empty, false)?;
    }
    let objects = Objects::open(&repo.setup);
    let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
    hooks.mark_symbolic("HEAD");
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::symref_update("HEAD", full.as_str(), None).no_deref(true))
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let cancelled = crate::commands::reset::remove_branch_state(&repo.setup.git_dir, repo.hash);
    if reflog {
        let logs = rgit_refs::reflog::FilesReflog::new(
            repo.setup.git_dir.clone(),
            repo.setup.common_dir.clone(),
            repo.hash,
        );
        logs.create(&full).map_err(|e| Die::Fatal(e.to_string()))?;
    }
    if !quiet {
        // `checkout --orphan` keeps the index and work tree and says
        // nothing about the old `HEAD`.
        if let Some(old) = detached_at.filter(|_| clear) {
            report_old_head(repo, &store, &objects, old, None)?;
        }
        eprintln!("{}", msg::switched_new(name));
    }
    warn_cancelled(cancelled, quiet);
    Ok(0)
}

/// Peels `id` to a commit.
pub fn peel_commit(objects: &Objects, id: &ObjectId) -> Option<ObjectId> {
    objects
        .peel(id)
        .filter(|p| objects.kind(p) == Some(ObjectKind::Commit))
}
