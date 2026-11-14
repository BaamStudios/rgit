// SPDX-License-Identifier: MIT
//! `stash apply`, `stash pop` and `stash branch`: merging the stashed
//! work tree into the current state with the merge engine (base: the
//! commit the entry was made on; ours: the current index; theirs: the
//! stashed work tree), restoring the untracked files and, with
//! `--index`, the stashed index.

use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Index, Stage};
use rgit_merge::{MergeResult, Options as MergeOptions};
use rgit_unpack::{Merge, Options as UnpackOptions, Porcelain, Worktree};
use rgit_worktree::stat::smudge_racy_entries;

use super::child::{Git, TempIndex};
use super::{Info, drop_entry, get_info, open, parse, refresh_index, say};
use crate::commands::read_tree::{lock_failed, path_options, unpack_failed};
use crate::messages::stash as msg;
use crate::prelude::*;
use crate::repository::Repo;

/// Conflict marker labels.
pub(crate) struct Labels {
    ours: String,
    theirs: String,
    base: String,
}

impl Default for Labels {
    fn default() -> Self {
        Labels {
            ours: msg::LABEL_OURS.to_owned(),
            theirs: msg::LABEL_THEIRS.to_owned(),
            base: msg::LABEL_BASE.to_owned(),
        }
    }
}

/// How an application ended.
enum Outcome {
    Clean,
    Conflicts,
    /// Failed; exit with this code.
    Failed(i32),
}

pub(crate) fn apply_cmd(ctx: &mut Context, args: &[String], pop: bool) -> CmdResult {
    let quiet = Cell::new(0);
    let index = Cell::new(-1);
    let ours = RefCell::new(None);
    let theirs = RefCell::new(None);
    let base = RefCell::new(None);
    let mut opts = vec![
        Opt::quiet(&quiet, msg::HELP_QUIET),
        Opt::bool(None, "index", &index, msg::HELP_INDEX),
    ];
    if !pop {
        opts.push(Opt::string(
            None,
            "label-ours",
            &ours,
            "label",
            msg::HELP_LABEL_OURS,
        ));
        opts.push(Opt::string(
            None,
            "label-theirs",
            &theirs,
            "label",
            msg::HELP_LABEL_THEIRS,
        ));
        opts.push(Opt::string(
            None,
            "label-base",
            &base,
            "label",
            msg::HELP_LABEL_BASE,
        ));
    }
    let usage = if pop { msg::USAGE_POP } else { msg::USAGE_APPLY };
    let mut parser = Parser::new(&[usage], opts);
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    drop(parser);
    let mut labels = Labels::default();
    if let Some(l) = ours.into_inner() {
        labels.ours = l;
    }
    if let Some(l) = theirs.into_inner() {
        labels.theirs = l;
    }
    if let Some(l) = base.into_inner() {
        labels.base = l;
    }
    let quiet = quiet.get() > 0;

    let mut repo = open(ctx)?;
    repo.work_tree()?;
    let info = get_info(&mut repo, &rest)?;
    if pop && !info.is_stash_ref {
        return Err(Die::Error(msg::not_stash_reference(&info.revision), 128));
    }
    let git = Git::new(ctx, &repo)?;
    let index = match index.get() {
        -1 => repo
            .config
            .bool("stash.index")
            .ok()
            .flatten()
            .unwrap_or(false),
        v => v > 0,
    };
    match do_apply(&repo, &git, &info, index, quiet, &labels)? {
        Outcome::Clean => {
            if pop {
                drop_entry(&repo, &info, quiet)?;
            }
            Ok(0)
        }
        Outcome::Conflicts => {
            if pop {
                say(msg::ENTRY_KEPT);
            }
            Ok(1)
        }
        Outcome::Failed(code) => {
            if pop {
                say(msg::ENTRY_KEPT);
            }
            Err(Die::Exit(code))
        }
    }
}

/// `git stash branch <name> [<stash>]`.
pub(crate) fn branch(ctx: &mut Context, args: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_BRANCH], vec![]);
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    drop(parser);
    let Some(name) = rest.first() else {
        eprintln!("{}", msg::NO_BRANCH_NAME);
        return Err(Die::Exit(128));
    };
    let mut repo = open(ctx)?;
    repo.work_tree()?;
    let info = get_info(&mut repo, &rest[1..])?;
    let git = Git::new(ctx, &repo)?;
    let code = git
        .cmd(&["checkout", "-b", name.as_str(), &info.b_commit.to_hex()])
        .status()?;
    if code != 0 {
        return Err(Die::Exit(code));
    }
    // The checkout changed HEAD and the index; read them again.
    let repo = open(ctx)?;
    match do_apply(&repo, &git, &info, true, false, &Labels::default())? {
        Outcome::Clean => {
            if info.is_stash_ref {
                drop_entry(&repo, &info, false)?;
            }
            Ok(0)
        }
        Outcome::Conflicts => Ok(1),
        Outcome::Failed(code) => Err(Die::Exit(code)),
    }
}

/// Appends `reset: moving to HEAD` to the reflog of `HEAD`.
fn log_head_reset(repo: &Repo) {
    let objects = crate::refs_support::Objects::open(&repo.setup);
    let hooks = crate::refs_support::UpdateHooks::new(&repo.setup, &repo.config, &objects);
    if let Some(head) = repo.refs.resolve("HEAD") {
        let message = crate::messages::reset::moving_to("HEAD");
        hooks.log("HEAD", head, head, &message, false);
    }
}

fn index_tree(repo: &Repo) -> Option<ObjectId> {
    let (index, _) = repo.read_index().ok()?;
    rgit_commit::write_tree(repo.odb(), &index, &Default::default()).ok()
}

fn merge(
    repo: &Repo,
    base: ObjectId,
    ours: ObjectId,
    theirs: ObjectId,
    labels: &Labels,
) -> Result<MergeResult, Die> {
    let opts = MergeOptions {
        branch1: labels.ours.as_bytes().to_vec(),
        branch2: labels.theirs.as_bytes().to_vec(),
        ancestor: labels.base.as_bytes().to_vec(),
        style: crate::commands::merge_file::config_style(&repo.config)?,
        ..MergeOptions::default()
    };
    rgit_merge::merge_trees(repo.odb(), &base, &ours, &theirs, &opts)
        .map_err(|e| Die::Fatal(e.to_string()))
}

fn do_apply(
    repo: &Repo,
    git: &Git,
    info: &Info,
    restore_index: bool,
    quiet: bool,
    labels: &Labels,
) -> Result<Outcome, Die> {
    if refresh_index(repo).is_err() {
        eprintln!("error: {}", msg::COULD_NOT_WRITE_INDEX);
        return Ok(Outcome::Failed(128));
    }
    let Some(c_tree) = index_tree(repo) else {
        eprintln!("error: {}", msg::IN_MIDDLE_OF_MERGE);
        return Ok(Outcome::Failed(128));
    };

    // The stashed index, carried over to the current one.
    let mut index_target = None;
    if restore_index {
        if info.b_tree == info.i_tree || c_tree == info.i_tree {
            index_target = Some(c_tree);
        } else {
            let r = merge(repo, info.b_tree, c_tree, info.i_tree, labels)?;
            if !r.clean {
                eprintln!("{}", msg::CONFLICTS_IN_INDEX);
                return Ok(Outcome::Failed(1));
            }
            index_target = Some(r.tree);
            // git records a reset of HEAD to itself at this point.
            log_head_reset(repo);
        }
    }

    let mut outcome = Outcome::Clean;
    let mut failed = None;
    if info.w_tree == info.b_tree {
        // Printed even with --quiet.
        say(msg::ALREADY_UP_TO_DATE);
    }
    if info.w_tree != info.b_tree {
        let result = merge(repo, info.b_tree, c_tree, info.w_tree, labels)?;
        match checkout_result(repo, c_tree, &result)? {
            Ok(()) => {
                if !quiet {
                    let mut out = Vec::new();
                    for m in &result.messages {
                        out.extend_from_slice(&m.text);
                        out.push(b'\n');
                    }
                    use std::io::Write;
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(&out);
                    let _ = o.flush();
                }
                if !result.clean {
                    outcome = Outcome::Conflicts;
                }
            }
            Err(code) => failed = Some(code),
        }
    }
    // The untracked files come back after the merge, even when it was
    // refused.
    if let Some(u) = info.u_tree {
        if !restore_untracked(repo, git, u)? {
            eprintln!("error: {}", msg::COULD_NOT_RESTORE_UNTRACKED);
            failed = failed.or(Some(128));
        }
    }

    if failed.is_none() && matches!(outcome, Outcome::Clean) {
        match index_target {
            Some(t) => set_index(repo, t)?,
            None => reset_index_keep_new(repo, c_tree)?,
        }
    }

    super::refresh_after(repo);
    if !quiet {
        git.cmd(&["status"])
            .in_dir(&repo.setup.original_cwd)
            .status()?;
    }
    Ok(match failed {
        Some(code) => Outcome::Failed(code),
        None => outcome,
    })
}

/// Moves the index and work tree from `c_tree` to the merge result,
/// leaving conflicted paths at stages 1 to 3. `Err(code)` when local
/// changes would be overwritten (the messages are printed).
fn checkout_result(
    repo: &Repo,
    c_tree: ObjectId,
    result: &MergeResult,
) -> Result<Result<(), i32>, Die> {
    let work_tree = repo.work_tree()?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options())
        .map_err(lock_failed)?;
    let (index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
    let wt = Worktree {
        root: &work_tree,
        checker: &checker,
    };
    let mut opts = UnpackOptions {
        update: true,
        show_all_errors: true,
        paths: path_options(repo),
        excludes: Some(&mut excludes),
        ..UnpackOptions::default()
    };
    let merge = Merge::TwoWay {
        old: c_tree,
        new: result.tree,
    };
    let plan = match rgit_unpack::plan(repo.odb(), &index, &merge, Some(&wt), &mut opts) {
        Ok(p) => p,
        Err(rgit_unpack::Error::Rejected(r)) => {
            for m in r.porcelain_messages(Porcelain::Merge) {
                eprintln!("error: {m}");
            }
            eprintln!("{}", rgit_unpack::messages::ABORTING);
            return Ok(Err(128));
        }
        Err(e) => return Err(unpack_failed(e)),
    };
    let conv = crate::convert::Converter::checkout(repo, plan.index())?;
    let applied = plan.apply(repo.odb(), &wt, &conv);
    let mut index = applied.index;
    repo.converter()?.use_index(&index);
    if !result.clean {
        let entries = result
            .index_entries(repo.odb())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        for c in &result.conflicts {
            index.remove_path(&c.path);
        }
        for e in entries {
            if e.stage().is_conflict() {
                index
                    .add(
                        e,
                        AddOptions {
                            ok_to_replace: true,
                            skip_df_check: true,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| Die::Fatal(e.to_string()))?;
            }
        }
    }
    if let Some(reference) = mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock)?;
    if !applied.errors.is_empty() {
        for e in &applied.errors {
            eprintln!("error: {e}");
        }
        return Ok(Err(128));
    }
    Ok(Ok(()))
}

/// Writes the files of the untracked tree `u` into the work tree; false
/// when one of them is in the way (reported by `checkout-index`).
fn restore_untracked(repo: &Repo, git: &Git, u: ObjectId) -> Result<bool, Die> {
    let tmp = TempIndex::new(repo, "stash-untracked-index");
    let code = git
        .cmd(&["read-tree", &u.to_hex()])
        .index(&tmp.path)
        .status()?;
    if code != 0 {
        return Ok(false);
    }
    let code = git
        .cmd(&["checkout-index", "--all"])
        .index(&tmp.path)
        .status()?;
    Ok(code == 0)
}

/// Without `--index`: the index goes back to `c_tree`, except that
/// files the stash added stay added.
fn reset_index_keep_new(repo: &Repo, c_tree: ObjectId) -> Result<(), Die> {
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options())
        .map_err(lock_failed)?;
    let (merged, mtime) = repo.read_index()?;
    let mut old = Vec::new();
    super::flatten(repo, &c_tree, b"", &mut old)?;
    old.sort_by(|a, b| a.0.cmp(&b.0));
    let in_old = |p: &[u8]| old.binary_search_by(|o| o.0.as_slice().cmp(p)).is_ok();
    let added: Vec<_> = merged
        .entries()
        .iter()
        .filter(|e| !in_old(e.path()))
        .cloned()
        .collect();
    // Keep the version and extensions of the merged index.
    let mut index: Index = merged.clone();
    index.retain(|_| false);
    for (path, mode, id) in old {
        let keep = merged
            .get(&path, Stage::MERGED)
            .filter(|e| e.mode.0 == mode && e.id == id)
            .cloned();
        let entry = keep.unwrap_or_else(|| {
            rgit_index::Entry::new(path, Stage::MERGED, rgit_index::Mode(mode), id)
        });
        index
            .add(
                entry,
                AddOptions {
                    ok_to_replace: true,
                    ..Default::default()
                },
            )
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    for e in added {
        index
            .add(
                e,
                AddOptions {
                    ok_to_replace: true,
                    ..Default::default()
                },
            )
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    index.set_cache_tree(None);
    if let Some(reference) = mtime {
        let checker = repo.checker(mtime)?;
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock)
}

/// With `--index`: the index becomes `tree` (stat data kept where the
/// entry does not change); the work tree is left alone.
fn set_index(repo: &Repo, tree: ObjectId) -> Result<(), Die> {
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options())
        .map_err(lock_failed)?;
    let (index, mtime) = repo.read_index()?;
    let mut opts = UnpackOptions {
        reset: true,
        ..UnpackOptions::default()
    };
    let plan = rgit_unpack::plan(repo.odb(), &index, &Merge::OneWay(tree), None, &mut opts)
        .map_err(unpack_failed)?;
    let mut index = plan.into_index();
    if let Some(reference) = mtime {
        let checker = repo.checker(mtime)?;
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock)
}
