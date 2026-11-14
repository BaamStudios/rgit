// SPDX-License-Identifier: MIT
//! Glue between the commands and `rgit-hook`: the hook directory
//! (`core.hooksPath` or `$GIT_DIR/hooks`), the directory hooks run in and
//! the environment they inherit (githooks(5)).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hook::{HookDir, Hooks, Outcome, RunOptions};
use rgit_repo::Setup;

use crate::{Context, Die};

/// Strips a leading `./` (git names `$GIT_DIR/hooks` of a bare repository
/// entered directly as `hooks/...`).
fn tidy(p: &Path) -> PathBuf {
    p.strip_prefix(".")
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| p.to_path_buf())
}

/// What hooks inherit from the command line: the environment of child
/// processes (`-c` settings, `--git-dir`, ...) and `--no-advice`.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    env: Vec<(OsString, OsString)>,
    no_advice: bool,
}

impl HookContext {
    pub fn new(ctx: &Context) -> Self {
        HookContext {
            env: ctx.child_env(),
            no_advice: ctx.no_advice,
        }
    }
}

/// The hooks of an opened repository.
pub fn of(repo: &crate::repo_objects::Repo) -> Result<Hooks, Die> {
    with_context(&repo.hook_context, &repo.setup, &repo.config)
}

/// The hooks of the repository of `setup`.
pub fn for_repo(ctx: &Context, setup: &Setup, config: &ConfigSet) -> Result<Hooks, Die> {
    with_context(&HookContext::new(ctx), setup, config)
}

/// The hooks of the repository of `setup`, for `hc`.
pub fn with_context(hc: &HookContext, setup: &Setup, config: &ConfigSet) -> Result<Hooks, Die> {
    // Hooks run at the top of the work tree, or in the git directory of a
    // bare repository.
    let run_dir = match &setup.work_tree {
        Some(w) => w.clone(),
        None if setup.git_dir == Path::new(".") => setup.cwd.clone(),
        None => setup.cwd.join(&setup.git_dir),
    };
    let env = SystemEnv::default();
    let custom = config
        .path("core.hookspath", &env)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let dir = match custom {
        Some(p) if p.is_absolute() => HookDir {
            shown: p.clone(),
            path: p,
        },
        Some(p) => HookDir {
            path: run_dir.join(&p),
            shown: p,
        },
        None => {
            let d = setup.common_dir.join("hooks");
            let path = setup.cwd.join(&d);
            let shown = if run_dir == setup.cwd {
                tidy(&d)
            } else {
                path.clone()
            };
            HookDir { shown, path }
        }
    };
    let mut child_env = hc.env.clone();
    // In a bare repository the hooks run in the git directory, and git
    // tells them so.
    if setup.work_tree.is_none()
        && !child_env.iter().any(|(k, _)| k == "GIT_DIR")
        && std::env::var_os("GIT_DIR").is_none()
    {
        child_env.push(("GIT_DIR".into(), ".".into()));
    }
    if let Some(p) = &setup.prefix {
        child_env.push(("GIT_PREFIX".into(), p.clone()));
    }
    let advisor = rgit_messages::Advisor::new(config, &env, hc.no_advice).map_err(Die::Fatal)?;
    Ok(Hooks::new(config, Some(dir), run_dir, child_env, advisor))
}

/// The configured hooks outside a repository (they run in `cwd`).
pub fn outside_repo(ctx: &Context, cwd: PathBuf, config: &ConfigSet) -> Result<Hooks, Die> {
    Ok(Hooks::new(
        config,
        None,
        cwd,
        ctx.child_env(),
        ctx.advisor(config)?,
    ))
}

/// Runs `event` with `opts`; a configuration error is fatal.
pub fn run(hooks: &Hooks, event: &str, opts: &RunOptions<'_>) -> Result<Outcome, Die> {
    hooks.run(event, opts).map_err(Die::Fatal)
}

/// Runs `post-checkout` with `<old> <new> <flag>` (githooks(5)); a
/// missing old commit is the null id. Returns the or-ed exit codes.
pub fn post_checkout(
    repo: &crate::repo_objects::Repo,
    old: Option<rgit_hash::ObjectId>,
    new: Option<rgit_hash::ObjectId>,
    branch: bool,
) -> Result<i32, Die> {
    let null = rgit_hash::ObjectId::null(repo.hash);
    let hooks = of(repo)?;
    let args = [
        old.unwrap_or(null).to_hex(),
        new.unwrap_or(null).to_hex(),
        if branch { "1" } else { "0" }.to_owned(),
    ];
    Ok(run(&hooks, "post-checkout", &RunOptions::with_args(args))?.code)
}
