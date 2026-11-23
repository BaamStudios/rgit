// SPDX-License-Identifier: MIT
//! Automatic maintenance (`git maintenance run --auto`,
//! Documentation/git-maintenance.adoc, Documentation/config/maintenance.adoc)
//! after the commands that write to the repository.
//!
//! git v2.56.1 runs it after `commit`, a `merge` that made a commit or
//! fast-forwarded, `fetch`, `am` once the series is done, and `rebase` when
//! the branch is already up to date or on `--abort` (not after a rebase
//! that replays commits; the commits the sequencer makes with `commit`
//! after a conflict count as commits). It is disabled by
//! `maintenance.auto=false`, without that setting by `gc.auto` of 0 or
//! less, and by `fetch --no-auto-maintenance`.
//!
//! The `gc` task (strategy `gc` or `maintenance.gc.enabled`) runs
//! `gc --auto`. Of the tasks of the default strategy (`geometric`: `commit-graph`,
//! `geometric-repack`, `reflog-expire`) only `reflog-expire` is done:
//! `reflog expire --all` once the `HEAD` reflog has
//! `maintenance.reflog-expire.auto` (100) entries old enough to go.
//! Writing commit-graphs and repacking are part of #97. The work happens in
//! the foreground; a held `objects/maintenance.lock` skips it silently.

use bstr::ByteSlice;
use rgit_config::ConfigSet;

use crate::messages::maintenance as msg;
use crate::prelude::*;
use crate::reflog_expire::{self, Explicit};
use crate::repository::Repo;

/// Runs the automatic maintenance of the current repository. Problems are
/// reported like a failing `git maintenance` child; they never change the
/// outcome of the calling command.
pub fn run_auto(ctx: &Context) {
    run_auto_quiet(ctx, false);
}

/// [`run_auto`] for a command run with `--quiet`, which git passes on to
/// `maintenance run --auto --quiet` (and from there to `gc --auto`).
pub fn run_auto_quiet(ctx: &Context, quiet: bool) {
    if let Err(e) = auto(ctx, quiet) {
        let _ = e.emit();
    }
}

/// Whether the `gc` task belongs to the configured strategy
/// (`maintenance.gc.enabled`, else strategy `gc`).
fn gc_enabled(config: &ConfigSet) -> Result<bool, Die> {
    if let Some(on) = config_bool(config, "maintenance.gc.enabled")? {
        return Ok(on);
    }
    let strategy = config
        .get("maintenance.strategy")
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned());
    Ok(strategy.as_deref() == Some("gc"))
}

/// The `gc` task: `gc --auto`, in the background unless
/// `maintenance.autoDetach` (else `gc.autoDetach`) is false.
fn run_gc_task(ctx: &Context, config: &ConfigSet, quiet: bool) -> Result<(), Die> {
    let detach = match config_bool(config, "maintenance.autodetach")? {
        Some(v) => v,
        None => config_bool(config, "gc.autodetach")?.unwrap_or(true),
    };
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut cmd = std::process::Command::new(exe);
    let args = [
        "gc",
        "--auto",
        if quiet { "--quiet" } else { "--no-quiet" },
        if detach { "--detach" } else { "--no-detach" },
    ];
    cmd.args(args);
    for (k, v) in ctx.child_env() {
        cmd.env(k, v);
    }
    let _ = rgit_trace::trace2::status(
        cmd.stdin(std::process::Stdio::null()),
        Default::default(),
        &rgit_trace::trace2::git_argv(&args),
    );
    Ok(())
}

fn config_bool(config: &ConfigSet, key: &str) -> Result<Option<bool>, Die> {
    config.bool(key).map_err(|e| Die::Fatal(e.to_string()))
}

/// Whether the `reflog-expire` task belongs to the configured strategy.
fn reflog_expire_enabled(config: &ConfigSet) -> Result<bool, Die> {
    if let Some(on) = config_bool(config, "maintenance.reflog-expire.enabled")? {
        return Ok(on);
    }
    let strategy = config
        .get("maintenance.strategy")
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned());
    match strategy.as_deref() {
        None | Some("geometric") => Ok(true),
        Some("gc" | "incremental") => Ok(false),
        Some(other) => Err(Die::Fatal(msg::unknown_strategy(other))),
    }
}

fn auto(ctx: &Context, quiet: bool) -> Result<(), Die> {
    let Some(repo) = Repo::open_gently(ctx)? else {
        return Ok(());
    };
    // maintenance.auto, else a gc.auto of 0 or less, switches it off.
    let enabled = match config_bool(&repo.config, "maintenance.auto")? {
        Some(on) => on,
        None => repo
            .config
            .int("gc.auto")
            .map_err(|e| Die::Fatal(e.to_string()))?
            .is_none_or(|n| n > 0),
    };
    if !enabled {
        return Ok(());
    }
    if gc_enabled(&repo.config)? {
        run_gc_task(ctx, &repo.config, quiet)?;
    }
    if !reflog_expire_enabled(&repo.config)? {
        return Ok(());
    }
    let threshold = repo
        .config
        .int("maintenance.reflog-expire.auto")
        .map_err(|e| Die::Fatal(e.to_string()))?
        .unwrap_or(100);
    if threshold == 0 {
        return Ok(());
    }
    let date = rgit_date::DateContext::from_env();
    let settings = reflog_expire::Config::load(&repo.config, Explicit::default(), &date)?;
    if threshold > 0 && (reflog_expire::expired_head_entries(&repo, &settings) as i64) < threshold {
        return Ok(());
    }
    let common = repo.setup.absolute_common_dir();
    let objects = crate::repo_objects::objects_dir(&repo.setup);
    let Ok(lock) = rgit_fs::LockFile::acquire(
        objects.join("maintenance"),
        &rgit_fs::LockOptions::default(),
    ) else {
        return Ok(());
    };
    let git_dir = repo.setup.absolute_git_dir();
    let mut result = Ok(());
    for log in reflog_expire::all_logs(&git_dir, &common, repo.hash, false) {
        let cut = settings.cutoffs(&log.name);
        result = reflog_expire::expire(&repo, &common, &log, cut, Default::default());
        if result.is_err() {
            break;
        }
    }
    lock.rollback();
    result
}
