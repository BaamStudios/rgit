// SPDX-License-Identifier: MIT
//! `git branch` (Documentation/git-branch.adoc, Documentation/config/branch.adoc).
//!
//! Implemented: listing (`-a`, `-r`, `--list <pattern>`, `--show-current`,
//! `-v`, `-vv` with upstream and ahead/behind counts, `--abbrev`),
//! creating `<name> [<start>]` with a reflog entry, deleting with `-d`
//! (merged into the upstream, else into `HEAD`) and `-D`, also `-r`;
//! renaming and copying with `-m`/`-M`/`-c`/`-C` (ref, reflog and the
//! `branch.<name>` configuration section), `-u`/`--set-upstream-to` and
//! `--unset-upstream`; upstream tracking of new branches (`-t`/`--track`,
//! `--no-track`, `branch.autoSetupMerge`, `branch.autoSetupRebase`, shared
//! with `switch`/`checkout` in [`crate::tracking`]).
//!
//! Not yet: `-f`, colors, columns, `--format`, `--sort`,
//! `--merged`, `--contains`, `--points-at`, `--edit-description`.

use std::io::Write;
use std::path::PathBuf;

use bstr::ByteSlice;
use rgit_config::write as config_write;
use rgit_config::{SetRequest, ValuePattern};
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::name::check_branch_name;
use rgit_refs::{IterItem, IterOptions, RefStore, RefUpdate, Target, Transaction};
use rgit_remote::{BranchConfig, Remotes, upstream_ref};
use rgit_repo::Setup;
use rgit_revwalk::walk::{CommitInfo, CommitSource, Walk, WalkOptions};

use crate::messages::branch as msg;
use crate::messages::tracking as tmsg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repo::{self, Mode};
use crate::tracking::{self, AutoSetup, Track};

/// What the command works on.
struct Ctx<'a> {
    setup: &'a Setup,
    config: &'a rgit_config::ConfigSet,
    advisor: rgit_messages::Advisor,
    store: FilesStore,
    objects: &'a Objects,
    revs: Revs,
    /// The branch `HEAD` points to (short name), also when unborn.
    current: Option<String>,
    quiet: bool,
    auto: AutoSetup,
}

impl Ctx<'_> {
    fn tracking_repo(&self) -> tracking::Repo<'_> {
        tracking::Repo {
            setup: self.setup,
            config: self.config,
            store: &self.store,
            revs: &self.revs,
        }
    }

    fn hooks(&self) -> UpdateHooks<'_> {
        UpdateHooks::new(self.setup, self.config, self.objects)
    }

    fn config_path(&self) -> PathBuf {
        self.setup.common_dir.join("config")
    }

    fn branch_oid(&self, name: &str) -> Option<ObjectId> {
        match self.store.read_raw(&format!("refs/heads/{name}")) {
            Ok(Some(Target::Object(oid))) => Some(oid),
            _ => None,
        }
    }

    fn worktree_path(&self) -> String {
        self.setup
            .work_tree
            .as_deref()
            .unwrap_or(&self.setup.git_dir)
            .display()
            .to_string()
    }

    fn abbrev(&self, oid: &ObjectId) -> String {
        self.objects.abbrev(oid, 7)
    }

    fn run_tx(&self, update: RefUpdate, hooks: &mut UpdateHooks<'_>) -> Result<(), Die> {
        let mut tx = Transaction::new(&self.store);
        tx.push(update)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    /// The upstream ref of branch `name` from its configuration.
    fn upstream_of(&self, name: &str) -> Option<String> {
        let b = BranchConfig::from_config(self.config, name.as_bytes()).ok()?;
        let remotes = Remotes::from_config(self.config).ok()?;
        upstream_ref(&remotes, Some(&self.setup.git_dir), Some(&b))
            .ok()
            .map(|r| r.to_str_lossy().into_owned())
    }

    /// Whether `HEAD` is a symbolic ref (not detached).
    fn head_attached(&self) -> bool {
        matches!(self.store.read_raw("HEAD"), Ok(Some(Target::Symbolic(_))))
    }

    fn config_set(&self, key: &str, value: Option<&str>) -> Result<(), Die> {
        let pattern = ValuePattern::Any;
        let req = SetRequest {
            key: key.as_bytes(),
            value: value.map(str::as_bytes),
            pattern: &pattern,
            all: true,
            comment: None,
        };
        match config_write::set_in_file(&self.config_path(), &Default::default(), &req) {
            Ok(()) => Ok(()),
            Err(e) if value.is_none() && e.exit_code() == 5 => Ok(()),
            Err(e) => Err(Die::Fatal(e.to_string())),
        }
    }
}

fn not_valid_name(ctx: &Ctx<'_>, name: &str) -> Die {
    eprintln!("fatal: {}", msg::not_valid_branch_name(name));
    match ctx.advisor.advise(Advice::RefSyntax, msg::HINT_REF_SYNTAX) {
        Ok(()) => Die::Exit(128),
        Err(e) => Die::Fatal(e),
    }
}

fn valid_name(name: &str) -> bool {
    name != "HEAD" && check_branch_name(name.as_bytes()).is_ok()
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let set_upstream: RefCell<Option<String>> = RefCell::new(None);
    let unset_upstream = Cell::new(false);
    let remotes = Cell::new(false);
    let abbrev: RefCell<Option<String>> = RefCell::new(None);
    let all = Cell::new(false);
    let delete = Cell::new(0);
    let rename = Cell::new(0);
    let copy = Cell::new(0);
    let list = Cell::new(false);
    let show_current = Cell::new(false);
    let force = Cell::new(false);
    let track = Cell::new(Track::Unspecified);
    // Generic options, then the branch actions (the order of `-h`).
    let mut opts = vec![
            Opt::group(msg::GROUP_GENERIC),
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Track::option(&track),
            Opt::string('u', "set-upstream-to", &set_upstream, "upstream", msg::HELP_SET_UPSTREAM),
    ];
    opts.extend([
            Opt::flag(None, "unset-upstream", &unset_upstream, msg::HELP_UNSET_UPSTREAM),
            Opt::flag('r', "remotes", &remotes, msg::HELP_REMOTES).noneg(),
            Opt::string(None, "abbrev", &abbrev, "n", msg::HELP_ABBREV).optional(Some("")),
            Opt::group(msg::GROUP_ACTIONS),
            Opt::flag('a', "all", &all, msg::HELP_ALL).noneg(),
            Opt::set_int('d', "delete", &delete, msg::HELP_DELETE, 1),
            Opt::set_int('D', None, &delete, msg::HELP_FORCE_DELETE, 2).noneg(),
            Opt::set_int('m', "move", &rename, msg::HELP_MOVE, 1),
            Opt::set_int('M', None, &rename, msg::HELP_FORCE_MOVE, 2).noneg(),
            Opt::set_int('c', "copy", &copy, msg::HELP_COPY, 1),
            Opt::set_int('C', None, &copy, msg::HELP_FORCE_COPY, 2).noneg(),
            Opt::flag('l', "list", &list, msg::HELP_LIST),
            Opt::flag(None, "show-current", &show_current, msg::HELP_SHOW_CURRENT),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
    ]);
    let mut parser = Parser::new(msg::USAGE, opts);
    let args = parser.parse(argv)?.args;
    let abbrev_len = match abbrev.borrow().as_deref() {
        None | Some("") => 7,
        Some(v) => match v.parse::<usize>() {
            Ok(0) => 0,
            Ok(n) => n.clamp(4, 64),
            Err(_) => 7,
        },
    };
    let no_abbrev = argv.iter().any(|a| a == "--no-abbrev");

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let advisor = ctx.advisor(&config)?;
    let auto = AutoSetup::from_config(&config)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = Revs::new(ctx, &setup)?;
    let head = store.read_raw("HEAD").ok().flatten();
    let current = match &head {
        Some(Target::Symbolic(t)) => t.strip_prefix("refs/heads/").map(str::to_owned),
        _ => None,
    };
    let c = Ctx {
        setup: &setup,
        config: &config,
        advisor,
        store,
        objects: &objects,
        revs,
        current,
        quiet: quiet.get() > 0,
        auto,
    };

    if delete.get() > 0 {
        if args.is_empty() {
            return Err(Die::Fatal(msg::BRANCH_NAME_REQUIRED.to_owned()));
        }
        let names = args
            .iter()
            .map(|a| interpret_name(&c, a, remotes.get()))
            .collect::<Result<Vec<_>, _>>()?;
        return delete_branches(&c, &names, delete.get() > 1 || force.get(), remotes.get());
    }
    if show_current.get() {
        if let Some(cur) = &c.current {
            println!("{cur}");
        }
        return Ok(0);
    }
    if rename.get() > 0 || copy.get() > 0 {
        let is_copy = rename.get() == 0;
        let force = force.get() || if is_copy { copy.get() > 1 } else { rename.get() > 1 };
        return rename_or_copy(&c, &args, is_copy, force);
    }
    if let Some(up) = set_upstream.borrow().clone() {
        return set_upstream_to(&c, &up, &args);
    }
    if unset_upstream.get() {
        return unset_upstream_of(&c, &args);
    }
    if !list.get() && !remotes.get() && !all.get() && verbose.get() == 0 && !args.is_empty() {
        return create(&c, &args, &parser, track.get(), force.get());
    }
    let abbrev_len = if no_abbrev { 0 } else { abbrev_len };
    list_branches(&c, &args, &head, remotes.get(), all.get(), verbose.get(), abbrev_len)
}

/// A branch name argument of `-d`/`-D`: `@{-<n>}` and
/// `[<branch>]@{upstream}` stand for the branch they name (`remote`:
/// below `refs/remotes/`).
fn interpret_name(c: &Ctx<'_>, arg: &str, remote: bool) -> Result<String, Die> {
    if !arg.contains("@{") {
        return Ok(arg.to_owned());
    }
    let d = c
        .revs
        .with_resolver(|r| r.dwim_ref(arg))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let prefix = if remote { "refs/remotes/" } else { "refs/heads/" };
    let full = if d.count == 1 {
        Some(d.full_name)
    } else {
        // An upstream that does not exist (any more) still names it.
        let lower = arg.to_ascii_lowercase();
        ["@{u}", "@{upstream}"]
            .iter()
            .find_map(|s| lower.strip_suffix(s).map(|b| &arg[..b.len()]))
            .and_then(|b| match b {
                "" => c.current.clone(),
                b => Some(b.to_owned()),
            })
            .and_then(|b| c.upstream_of(&b))
    };
    Ok(match full.as_deref().and_then(|f| f.strip_prefix(prefix)) {
        Some(short) => short.to_owned(),
        None => arg.to_owned(),
    })
}

fn create(c: &Ctx<'_>, args: &[String], parser: &Parser<'_>, track: Track, force: bool) -> CmdResult {
    if args.len() > 2 {
        return Err(parser.usage_error().into());
    }
    let name = &args[0];
    if !valid_name(name) {
        return Err(not_valid_name(c, name));
    }
    let refname = format!("refs/heads/{name}");
    let existing = match c.store.read_raw(&refname) {
        Ok(Some(_)) if !force => return Err(Die::Fatal(msg::already_exists(name))),
        Ok(Some(_)) => c.store.resolve_oid(&refname).ok().flatten(),
        _ => None,
    };
    if existing.is_some() && c.current.as_deref() == Some(name.as_str()) && c.head_attached() {
        return Err(Die::Fatal(msg::cannot_force_update_checked_out(
            name,
            &c.worktree_path(),
        )));
    }
    let start = match args.get(1) {
        Some(s) => s.clone(),
        None => tracking::default_start(&c.store),
    };
    let start = start.as_str();
    let oid = c
        .revs
        .resolve(start)
        .and_then(|id| c.objects.peel(&id).or(Some(id)));
    let repo = c.tracking_repo();
    let plan = tracking::plan(&repo, name, start, oid.is_some(), track, &c.auto)?;
    let oid = oid.ok_or_else(|| Die::Fatal(msg::not_valid_object_name(start)))?;
    let mut hooks = c.hooks();
    let update = match existing {
        Some(old) => RefUpdate::update(refname.as_str(), oid, Some(old)).message(msg::reset_to(start)),
        None => RefUpdate::create(refname.as_str(), oid).message(msg::created_from(start)),
    };
    c.run_tx(update.create_reflog(true), &mut hooks)?;
    if let Some(plan) = plan {
        plan.apply(&repo, name, &c.auto, c.quiet)?;
    }
    Ok(0)
}

fn delete_branches(c: &Ctx<'_>, args: &[String], force: bool, remote: bool) -> CmdResult {
    let hash = c.setup.format.object_format;
    let head_oid = c.revs.resolve("HEAD");
    let mut code = 0;
    for name in args {
        let refname = if remote {
            format!("refs/remotes/{name}")
        } else {
            format!("refs/heads/{name}")
        };
        if !remote && c.current.as_deref() == Some(name.as_str()) {
            eprintln!("error: {}", msg::cannot_delete_checked_out(name, &c.worktree_path()));
            code = 1;
            continue;
        }
        let oid = match c.store.read_raw(&refname) {
            Ok(Some(Target::Object(oid))) => oid,
            _ => {
                if remote {
                    eprintln!("error: {}", msg::remote_not_found(name));
                } else {
                    eprintln!("error: {}", msg::not_found(name));
                }
                code = 1;
                continue;
            }
        };
        if !force && !remote && !merged_enough(c, name, &oid, head_oid.as_ref()) {
            eprintln!("error: {}", msg::not_fully_merged(name));
            c.advisor
                .advise(Advice::ForceDeleteBranch, &msg::hint_force_delete(name))
                .map_err(Die::Fatal)?;
            code = 1;
            continue;
        }
        let mut hooks = c.hooks();
        let mut tx = Transaction::new(&c.store);
        let r = tx
            .push(RefUpdate::delete(refname.as_str(), hash, Some(oid)).no_deref(true))
            .and_then(|()| tx.commit(&mut hooks));
        if let Err(e) = r {
            eprintln!("error: {e}");
            code = 1;
            continue;
        }
        if !remote {
            let _ = config_write::rename_section_in_file(
                &c.config_path(),
                &Default::default(),
                format!("branch.{name}").as_bytes(),
                None,
            );
        }
        if !c.quiet {
            let line = if remote {
                msg::deleted_remote(name, &c.abbrev(&oid))
            } else {
                msg::deleted(name, &c.abbrev(&oid))
            };
            println!("{line}");
        }
    }
    Ok(code)
}

/// `branch -d`: the branch must be merged into its upstream, or into
/// `HEAD` when it has none. Warns when the two answers differ.
fn merged_enough(c: &Ctx<'_>, name: &str, oid: &ObjectId, head: Option<&ObjectId>) -> bool {
    let upstream = c
        .upstream_of(name)
        .and_then(|r| c.revs.resolve(&r).map(|o| (r, o)));
    let head_merged = head.is_some_and(|h| c.revs.is_ancestor(oid, h));
    let Some((up_name, up_oid)) = upstream else {
        return head_merged;
    };
    let merged = c.revs.is_ancestor(oid, &up_oid);
    if head.is_some() && merged != head_merged {
        if merged {
            eprintln!("warning: {}", msg::merged_to_upstream_not_head(name, &up_name));
        } else {
            eprintln!("warning: {}", msg::merged_to_head_not_upstream(name, &up_name));
        }
    }
    merged
}

fn rename_or_copy(c: &Ctx<'_>, args: &[String], is_copy: bool, force: bool) -> CmdResult {
    let (old, new) = match args.len() {
        0 => return Err(Die::Fatal(msg::BRANCH_NAME_REQUIRED.to_owned())),
        1 => match &c.current {
            Some(cur) => (cur.clone(), args[0].clone()),
            None if is_copy => return Err(Die::Fatal(msg::COPY_DETACHED.to_owned())),
            None => return Err(Die::Fatal(msg::RENAME_DETACHED.to_owned())),
        },
        2 => (args[0].clone(), args[1].clone()),
        _ if is_copy => return Err(Die::Fatal(msg::TOO_MANY_COPY.to_owned())),
        _ => return Err(Die::Fatal(msg::TOO_MANY_RENAME.to_owned())),
    };
    let old_ref = format!("refs/heads/{old}");
    let new_ref = format!("refs/heads/{new}");
    let is_current = c.current.as_deref() == Some(old.as_str());
    let oid = c.branch_oid(&old);
    if oid.is_none() {
        if is_current && is_copy {
            return Err(Die::Fatal(msg::no_commit_on_branch(&old)));
        }
        if !is_current {
            return Err(Die::Fatal(msg::no_branch_named(&old)));
        }
    }
    if !valid_name(&new) {
        return Err(not_valid_name(c, &new));
    }
    let target = c.branch_oid(&new);
    if target.is_some() && old != new {
        if !force {
            return Err(Die::Fatal(msg::already_exists(&new)));
        }
        if c.current.as_deref() == Some(new.as_str()) {
            return Err(Die::Fatal(msg::cannot_force_update_checked_out(
                &new,
                &c.worktree_path(),
            )));
        }
    }

    let Some(oid) = oid else {
        // Unborn current branch: only HEAD changes.
        let mut hooks = c.hooks().without_reflogs();
        hooks.mark_symbolic("HEAD");
        c.run_tx(
            RefUpdate::symref_update("HEAD", new_ref.as_str(), None).no_deref(true),
            &mut hooks,
        )?;
        return Ok(0);
    };
    let message = if is_copy {
        msg::copied_reflog(&old_ref, &new_ref)
    } else {
        msg::renamed_reflog(&old_ref, &new_ref)
    };
    let hash = c.setup.format.object_format;
    let hooks = c.hooks();
    let logs = hooks.reflogs().cloned();

    // A forced target goes away first, with its log.
    if let Some(t) = target {
        if old != new {
            let mut quiet = c.hooks();
            c.run_tx(
                RefUpdate::delete(new_ref.as_str(), hash, Some(t)).no_deref(true),
                &mut quiet,
            )?;
        }
    }
    if let Some(logs) = &logs {
        if is_copy {
            if old != new {
                if let Ok(Some(data)) = logs.read_raw(&old_ref) {
                    let path = logs.path(&new_ref);
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    std::fs::write(&path, data).map_err(|e| Die::Fatal(e.to_string()))?;
                }
            }
        } else {
            logs.rename(&old_ref, &new_ref)
                .map_err(|e| Die::Fatal(e.to_string()))?;
        }
    }
    if !is_copy && old != new {
        // Deleting the old branch notes the change in HEAD's log when HEAD
        // is on it.
        let mut h = c.hooks();
        c.run_tx(
            RefUpdate::delete(old_ref.as_str(), hash, Some(oid))
                .no_deref(true)
                .message(message.as_str()),
            &mut h,
        )?;
        // Its log was moved above; the delete must not take it along.
    }
    if old != new {
        let mut silent = c.hooks().without_reflogs();
        c.run_tx(
            RefUpdate::create(new_ref.as_str(), oid).no_deref(true),
            &mut silent,
        )?;
    }
    let create_log = logs
        .as_ref()
        .is_some_and(|l| l.exists(&new_ref) || l.exists(&old_ref))
        || log_all_ref_updates(c);
    hooks.log(&new_ref, oid, oid, &message, create_log);
    if !is_copy && is_current {
        let mut silent = c.hooks().without_reflogs();
        silent.mark_symbolic("HEAD");
        c.run_tx(
            RefUpdate::symref_update("HEAD", new_ref.as_str(), None).no_deref(true),
            &mut silent,
        )?;
        if old != new {
            hooks.log("HEAD", ObjectId::null(hash), oid, &message, false);
        }
    }

    let old_section = format!("branch.{old}");
    let new_section = format!("branch.{new}");
    if old != new {
        let path = c.config_path();
        let r = if is_copy {
            config_write::copy_section_in_file(
                &path,
                &Default::default(),
                old_section.as_bytes(),
                new_section.as_bytes(),
            )
        } else {
            config_write::rename_section_in_file(
                &path,
                &Default::default(),
                old_section.as_bytes(),
                Some(new_section.as_bytes()),
            )
        };
        if let Err(e) = r {
            if !matches!(
                e.edit_error(),
                Some(rgit_config::EditError::NoSuchSection(_))
            ) {
                return Err(Die::Fatal(e.to_string()));
            }
        }
    }
    Ok(0)
}

fn log_all_ref_updates(c: &Ctx<'_>) -> bool {
    match c.config.bool("core.logallrefupdates") {
        Ok(Some(v)) => v,
        _ => c.setup.work_tree.is_some(),
    }
}

/// `-u <upstream> [<branch>]`.
fn set_upstream_to(c: &Ctx<'_>, upstream: &str, args: &[String]) -> CmdResult {
    let branch = match args.len() {
        0 => match &c.current {
            Some(cur) => {
                if c.branch_oid(cur).is_none() {
                    return Err(Die::Fatal(msg::no_commit_on_branch(cur)));
                }
                cur.clone()
            }
            None => return Err(Die::Fatal(msg::set_upstream_detached(upstream))),
        },
        1 => {
            if c.branch_oid(&args[0]).is_none() {
                if c.current.as_deref() == Some(args[0].as_str()) {
                    return Err(Die::Fatal(msg::no_commit_on_branch(&args[0])));
                }
                return Err(Die::Fatal(msg::branch_does_not_exist(&args[0])));
            }
            args[0].clone()
        }
        _ => return Err(Die::Fatal(msg::TOO_MANY_SET_UPSTREAM.to_owned())),
    };

    let repo = c.tracking_repo();
    let resolves = c.revs.resolve(upstream).is_some();
    let Some(plan) = tracking::plan(&repo, &branch, upstream, resolves, Track::Direct, &c.auto)?
    else {
        return Err(Die::Fatal(tmsg::not_a_branch(upstream)));
    };
    if plan.is_own_upstream(&branch) {
        eprintln!("warning: {}", tmsg::own_upstream(&branch));
        return Ok(0);
    }
    plan.apply(&repo, &branch, &c.auto, c.quiet)?;
    Ok(0)
}

fn unset_upstream_of(c: &Ctx<'_>, args: &[String]) -> CmdResult {
    let branch = match args.len() {
        0 => match &c.current {
            Some(cur) => cur.clone(),
            None => return Err(Die::Fatal(msg::UNSET_UPSTREAM_DETACHED.to_owned())),
        },
        1 => args[0].clone(),
        _ => return Err(Die::Fatal(msg::TOO_MANY_UNSET_UPSTREAM.to_owned())),
    };
    let has = c
        .config
        .string(format!("branch.{branch}.merge"))
        .ok()
        .flatten()
        .is_some();
    if !has {
        return Err(Die::Fatal(msg::no_upstream_information(&branch)));
    }
    c.config_set(&format!("branch.{branch}.remote"), None)?;
    c.config_set(&format!("branch.{branch}.merge"), None)?;
    Ok(0)
}

/// A walk source over the object database.
struct Commits<'a>(&'a Objects);

impl CommitSource for Commits<'_> {
    type Error = ();
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ()> {
        let (kind, data) = self.0.read(id).ok_or(())?;
        if kind != ObjectKind::Commit {
            return Err(());
        }
        let commit = Commit::parse(self.0.hash(), &data).map_err(|_| ())?;
        Ok(CommitInfo {
            parents: commit.parents().map_err(|_| ())?,
            time: commit
                .committer()
                .and_then(|s| s.time())
                .map_or(0, |t| t.seconds),
        })
    }
}

/// Commits reachable from `a` but not from `b`.
fn count_only(objects: &Objects, a: &ObjectId, b: &ObjectId) -> usize {
    let mut walk = Walk::new(Commits(objects), WalkOptions::default());
    if walk.push(a, false).is_err() || walk.push(b, true).is_err() {
        return 0;
    }
    walk.collect_all().map(|v| v.len()).unwrap_or(0)
}

/// The subject of a commit: its first paragraph on one line.
fn subject(objects: &Objects, id: &ObjectId) -> String {
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

/// Short form of a full ref name for display.
fn short_ref(full: &str) -> &str {
    full.strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/remotes/"))
        .or_else(|| full.strip_prefix("refs/tags/"))
        .or_else(|| full.strip_prefix("refs/"))
        .unwrap_or(full)
}

/// ` [<upstream>: ahead 1, behind 2]` and friends.
fn tracking(c: &Ctx<'_>, branch: &str, oid: &ObjectId, verbose: i32) -> String {
    let Some(up) = c.upstream_of(branch) else {
        return String::new();
    };
    let name = short_ref(&up);
    let up_oid = match c.store.read_raw(&up) {
        Ok(Some(Target::Object(o))) => Some(o),
        Ok(Some(Target::Symbolic(_))) => c.revs.resolve(&up),
        _ => None,
    };
    let Some(up_oid) = up_oid else {
        return if verbose > 1 {
            format!("[{name}: {}] ", msg::GONE)
        } else {
            format!("[{}] ", msg::GONE)
        };
    };
    let ahead = count_only(c.objects, oid, &up_oid);
    let behind = count_only(c.objects, &up_oid, oid);
    let counts = match (ahead, behind) {
        (0, 0) => String::new(),
        (a, 0) => msg::ahead(a),
        (0, b) => msg::behind(b),
        (a, b) => msg::ahead_behind(a, b),
    };
    match (verbose > 1, counts.is_empty()) {
        (true, true) => format!("[{name}] "),
        (true, false) => format!("[{name}: {counts}] "),
        (false, true) => String::new(),
        (false, false) => format!("[{counts}] "),
    }
}

enum Line {
    Detached(ObjectId),
    Ref {
        shown: String,
        current: bool,
        branch: Option<String>,
        target: Target,
    },
}

fn list_branches(
    c: &Ctx<'_>,
    patterns: &[String],
    head: &Option<Target>,
    remotes: bool,
    all: bool,
    verbose: i32,
    abbrev: usize,
) -> CmdResult {
    let local = !remotes || all;
    let remote = remotes || all;
    let mut lines: Vec<Line> = Vec::new();
    if let (true, Some(Target::Object(oid))) = (local, head) {
        lines.push(Line::Detached(*oid));
    }
    let mut prefixes = Vec::new();
    if local {
        prefixes.push("refs/heads/");
    }
    if remote {
        prefixes.push("refs/remotes/");
    }
    for prefix in prefixes {
        let items = c
            .store
            .iter(&IterOptions::prefix(prefix))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        for item in items {
            let IterItem::Ref(r) = item else { continue };
            let short = &r.name[prefix.len()..];
            let shown = if prefix == "refs/remotes/" && all {
                format!("remotes/{short}")
            } else {
                short.to_owned()
            };
            if !patterns.is_empty()
                && !patterns
                    .iter()
                    .any(|p| wildmatch(p.as_bytes(), shown.as_bytes(), WildmatchFlags::default()))
            {
                continue;
            }
            let is_local = prefix == "refs/heads/";
            lines.push(Line::Ref {
                current: is_local && c.current.as_deref() == Some(short),
                branch: is_local.then(|| short.to_owned()),
                shown,
                target: r.target.clone(),
            });
        }
    }

    let full_hex = |o: &ObjectId| {
        if abbrev == 0 {
            o.to_hex()
        } else {
            c.objects.abbrev(o, abbrev)
        }
    };
    let width = if verbose > 0 {
        lines
            .iter()
            .map(|l| match l {
                Line::Detached(o) => msg::detached_at(&c.abbrev(o)).chars().count(),
                Line::Ref { shown, .. } => shown.chars().count(),
            })
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let mut out = std::io::stdout().lock();
    for line in &lines {
        let text = match line {
            Line::Detached(oid) => {
                let label = msg::detached_at(&c.abbrev(oid));
                if verbose > 0 {
                    format!(
                        "* {label:<width$} {} {}",
                        full_hex(oid),
                        subject(c.objects, oid)
                    )
                } else {
                    format!("* {label}")
                }
            }
            Line::Ref {
                shown,
                current,
                branch,
                target,
            } => {
                let marker = if *current { '*' } else { ' ' };
                match target {
                    Target::Symbolic(t) => {
                        let t = t
                            .strip_prefix("refs/remotes/")
                            .or_else(|| t.strip_prefix("refs/heads/"))
                            .unwrap_or(t);
                        format!("{marker} {shown} -> {t}")
                    }
                    Target::Object(oid) if verbose > 0 => {
                        let track = branch
                            .as_deref()
                            .map(|b| tracking(c, b, oid, verbose))
                            .unwrap_or_default();
                        format!(
                            "{marker} {shown:<width$} {} {track}{}",
                            full_hex(oid),
                            subject(c.objects, oid)
                        )
                    }
                    Target::Object(_) => format!("{marker} {shown}"),
                }
            }
        };
        writeln!(out, "{text}").map_err(|e| Die::Fatal(e.to_string()))?;
    }
    Ok(0)
}
