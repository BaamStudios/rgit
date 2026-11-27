// SPDX-License-Identifier: MIT
//! The apply back end of `git rebase` (`--apply`, `-C`, `--whitespace`;
//! Documentation/git-rebase.adoc): the commits are turned into patches
//! with `format-patch` and replayed by `am --rebasing`, whose session in
//! `.git/rebase-apply` also holds rebase's `head-name`, `onto`,
//! `orig-head`, `quiet` and `autostash`. `--continue` and `--skip` go on
//! with `am`; the branch is moved when `am` is done. The child commands,
//! their options and the messages were taken from the behaviour of the
//! `git` binary (`GIT_TRACE`).

use crate::argv::CommandArgs as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

use rgit_hash::ObjectId;

use super::{Repo, State, apply_autostash, attach_head, detach_head, git_dir, io_die, write};
use crate::messages::rebase as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::sequencer;

/// A new rebase with the apply back end.
pub(crate) struct Start<'a> {
    /// The commits after this one are replayed (`None`: from the root).
    pub restrict: Option<ObjectId>,
    pub state: &'a State,
    pub am_opts: &'a [String],
}

fn exe() -> Result<std::path::PathBuf, Die> {
    std::env::current_exe().map_err(io_die)
}

/// The `am` options that follow from the rebase's.
fn am_args(state: &State) -> Vec<String> {
    let mut a = Vec::new();
    if state.quiet {
        a.push("-q".to_owned());
    }
    if state.signoff {
        a.push("--signoff".to_owned());
    }
    if state.cdate_is_adate {
        a.push("--committer-date-is-author-date".to_owned());
    }
    if state.ignore_date {
        a.push("--ignore-date".to_owned());
    }
    if let Some(s) = &state.gpg_sign {
        a.push(s.to_opt());
    }
    match state.rerere {
        Some(true) => a.push("--rerere-autoupdate".to_owned()),
        Some(false) => a.push("--no-rerere-autoupdate".to_owned()),
        None => {}
    }
    a
}

fn run_am(ctx: &Context, args: &[String], stdin: Option<&[u8]>) -> Result<i32, Die> {
    let _ = std::io::stdout().flush();
    let mut cmd = Command::new(exe()?);
    cmd.raw_arg("am")
        .raw_args(args)
        .raw_arg(format!("--resolvemsg={}", msg::APPLY_RESOLVE_MSG))
        .envs(ctx.child_env())
        .env(
            "GIT_REFLOG_ACTION",
            format!("{} (pick)", super::reflog_action()),
        );
    let status = match stdin {
        Some(data) => {
            let mut child = cmd.stdin(Stdio::piped()).spawn().map_err(io_die)?;
            if let Some(mut i) = child.stdin.take() {
                let _ = i.write_all(data);
            }
            child.wait().map_err(io_die)?
        }
        None => cmd.status().map_err(io_die)?,
    };
    Ok(status.code().unwrap_or(128))
}

/// Writes rebase's files into the `am` session.
fn write_state(repo: &Repo, state: &State) -> Result<(), Die> {
    let dir = git_dir(repo).join("rebase-apply");
    if !dir.is_dir() {
        return Ok(());
    }
    write(&dir.join("head-name"), format!("{}\n", state.head_name))?;
    write(&dir.join("onto"), format!("{}\n", state.onto.to_hex()))?;
    write(
        &dir.join("orig-head"),
        format!("{}\n", state.orig_head.to_hex()),
    )?;
    if state.quiet {
        write(&dir.join("quiet"), "")?;
    }
    if let Some(a) = &state.autostash {
        write(&dir.join("autostash"), format!("{a}\n"))?;
    }
    if let Some(r) = state.rerere {
        let opt = if r {
            "--rerere-autoupdate"
        } else {
            "--no-rerere-autoupdate"
        };
        write(&dir.join("allow_rerere_autoupdate"), format!("{opt}\n"))?;
    }
    Ok(())
}

/// Replays the commits after `HEAD` was detached at the new base.
pub(crate) fn start(ctx: &mut Context, s: &Start<'_>) -> CmdResult {
    let range = match s.restrict {
        Some(r) => vec![format!("{}...{}", r.to_hex(), s.state.orig_head.to_hex())],
        None => vec!["--root".to_owned(), s.state.orig_head.to_hex()],
    };
    let mut args: Vec<String> = [
        "format-patch",
        "-k",
        "--stdout",
        "--full-index",
        "--cherry-pick",
        "--right-only",
        "--default-prefix",
        "--no-renames",
        "--no-cover-letter",
        "--pretty=mboxrd",
        "--topo-order",
        "--no-base",
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend(range);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, patches) = super::run_child(ctx, &refs, true)?;
    if code != 0 {
        return Ok(code);
    }
    if patches.is_empty() {
        return finish(ctx, s.state);
    }
    // The patches go through `.git/rebased-patches`, as in git.
    let repo = Repo::open(ctx, Mode::Required)?;
    let file = git_dir(&repo).join("rebased-patches");
    let shown = repo.setup.git_dir.join("rebased-patches");
    drop(repo);
    if let Err(e) = std::fs::write(&file, &patches) {
        eprintln!(
            "error: {}",
            msg::could_not_write(&shown.to_string_lossy(), &crate::global::strerror(&e))
        );
        return Ok(1);
    }
    let mut am = vec!["--rebasing".to_owned(), "--patch-format=mboxrd".to_owned()];
    am.extend(am_args(s.state));
    am.extend(s.am_opts.iter().cloned());
    am.push(file.to_string_lossy().into_owned());
    let code = run_am(ctx, &am, None)?;
    let _ = std::fs::remove_file(&file);
    after_am(ctx, s.state, code)
}

/// After `am` ran: done, or stopped (rebase's files go into its session).
fn after_am(ctx: &mut Context, state: &State, code: i32) -> CmdResult {
    let repo = Repo::open(ctx, Mode::Required)?;
    let dir = git_dir(&repo).join("rebase-apply");
    if code == 0 && !dir.is_dir() {
        drop(repo);
        return finish(ctx, state);
    }
    write_state(&repo, state)?;
    Ok(if code == 0 { 0 } else { 1 })
}

/// `--continue` and `--skip` of a rebase with the apply back end.
pub(crate) fn resume(ctx: &mut Context, state: &State, how: &str) -> CmdResult {
    let mut args = vec![how.to_owned()];
    if state.quiet {
        args.push("-q".to_owned());
    }
    let code = run_am(ctx, &args, None)?;
    after_am(ctx, state, code)
}

/// Moves the branch to the replayed commits and goes back to it.
pub(crate) fn finish(ctx: &mut Context, state: &State) -> CmdResult {
    let mut repo = Repo::open(ctx, Mode::Required)?;
    let head = sequencer::head_commit(&mut repo)
        .ok_or_else(|| Die::Fatal(crate::messages::sequencer::could_not_read("HEAD")))?;
    let action = super::reflog_action();
    let head_name = &state.head_name;
    if head_name.starts_with("refs/") {
        let what = msg::reflog(
            &action,
            "finish",
            &format!("{head_name} onto {}", state.onto.to_hex()),
        );
        let old = repo.refs.resolve(head_name);
        let update = match old {
            Some(o) => rgit_refs::RefUpdate::update(head_name.as_str(), head, Some(o)),
            None => rgit_refs::RefUpdate::create(head_name.as_str(), head),
        };
        super::ref_tx(ctx, &repo, update.message(what), false)?;
        let what = msg::reflog(&action, "finish", &msg::returning_to(head_name));
        attach_head(ctx, &repo, head_name, &what)?;
    } else {
        let what = msg::reflog(&action, "finish", &msg::returning_to(&head.to_hex()));
        detach_head(ctx, &repo, head, &what)?;
    }
    let _ = std::fs::remove_dir_all(git_dir(&repo).join("rebase-apply"));
    drop(repo);
    crate::maintenance::run_auto(ctx);
    if let Some(a) = &state.autostash {
        apply_autostash(ctx, a)?;
    }
    Ok(0)
}
