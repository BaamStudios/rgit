// SPDX-License-Identifier: MIT
//! Repository maintenance (Documentation/git-maintenance.adoc,
//! Documentation/config/maintenance.adoc): the tasks of
//! `git maintenance run` ([`tasks`]), the background schedulers
//! ([`schedule`]) and the automatic maintenance after the commands that
//! write to the repository.
//!
//! git v2.56.1 runs `git maintenance run --auto` after `commit`, a `merge`
//! that made a commit or fast-forwarded, `fetch`, `am` once the series is
//! done, and `rebase` when the branch is already up to date or on
//! `--abort`. It is disabled by `maintenance.auto=false`, without that
//! setting by `gc.auto` of 0 or less, and by `fetch --no-auto-maintenance`.
//! The child gets `--detach` unless `maintenance.autoDetach` (else
//! `gc.autoDetach`, else `GIT_TEST_MAINT_AUTO_DETACH`) is false.

pub mod schedule;
pub mod tasks;

use rgit_config::ConfigSet;

use crate::prelude::*;
use crate::repository::Repo;

/// Runs the automatic maintenance of the current repository. Problems are
/// reported like a failing `git maintenance` child; they never change the
/// outcome of the calling command.
pub fn run_auto(ctx: &Context) {
    run_auto_quiet(ctx, false);
}

/// [`run_auto`] for a command run with `--quiet`, which git passes on as
/// `maintenance run --auto --quiet`.
pub fn run_auto_quiet(ctx: &Context, quiet: bool) {
    let Ok(Some(repo)) = Repo::open_gently(ctx) else {
        return;
    };
    if !auto_enabled(&repo.config) {
        return;
    }
    let detach = auto_detach(&repo.config);
    drop(repo);
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args = [
        "maintenance",
        "run",
        "--auto",
        if quiet { "--quiet" } else { "--no-quiet" },
        if detach { "--detach" } else { "--no-detach" },
    ];
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null());
    for (k, v) in ctx.child_env() {
        cmd.env(k, v);
    }
    let _ = rgit_trace::trace2::status(
        &mut cmd,
        Default::default(),
        &rgit_trace::trace2::git_argv(&args),
    );
}

/// `maintenance.auto`, else a `gc.auto` above 0.
fn auto_enabled(config: &ConfigSet) -> bool {
    match config.bool("maintenance.auto").ok().flatten() {
        Some(on) => on,
        None => config.int("gc.auto").ok().flatten().is_none_or(|n| n > 0),
    }
}

/// Whether the automatic maintenance continues in the background.
fn auto_detach(config: &ConfigSet) -> bool {
    if let Some(v) = config.bool("maintenance.autodetach").ok().flatten() {
        return v;
    }
    if let Some(v) = config.bool("gc.autodetach").ok().flatten() {
        return v;
    }
    std::env::var("GIT_TEST_MAINT_AUTO_DETACH")
        .ok()
        .and_then(|v| rgit_config::value::parse_bool(Some(v.as_bytes())))
        .unwrap_or(true)
}
