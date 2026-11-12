// SPDX-License-Identifier: MIT
//! Repository setup for commands: runs [`rgit_repo::setup`] with the
//! environment and global options, prints warnings and errors the way git
//! does, and moves into the directory the command should run in.

use std::io::Write;

use rgit_config::{CommandLine, SystemEnv};
use rgit_repo::{Error, Setup, SetupOptions, StandardProtectedConfig};

use crate::{Context, Die};

/// How a command uses the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A repository is required (`RUN_SETUP`); `GIT_TRACE_SETUP` is written.
    Required,
    /// A repository is required, without setup tracing.
    RequiredQuiet,
}

/// The `-c` / `--config-env` settings as a configuration command line.
pub fn command_line(ctx: &Context) -> CommandLine {
    let mut cl = CommandLine::default();
    for (k, v) in &ctx.config_parameters {
        match v {
            Some(v) => cl.push_c(format!("{k}={v}")),
            None => cl.push_c(k),
        }
    }
    cl
}

/// Set up the repository, change into [`Setup::cwd`] and return the setup.
pub fn setup(ctx: &Context, mode: Mode) -> Result<Setup, Die> {
    let env = SystemEnv::default();
    let mut opts = SetupOptions::from_process().map_err(die)?;
    if let Some(d) = &ctx.git_dir {
        opts.git_dir = Some(d.into());
    }
    if let Some(w) = &ctx.work_tree {
        opts.work_tree = Some(w.into());
    }
    if ctx.bare {
        opts.implicit_work_tree = false;
    }
    let mut protected = StandardProtectedConfig::new(&env, command_line(ctx));
    let setup = rgit_repo::setup(&opts, &mut protected).map_err(die)?;
    let mut err = std::io::stderr();
    for w in &setup.warnings {
        let _ = writeln!(err, "warning: {w}");
    }
    if setup.cwd != opts.cwd {
        std::env::set_current_dir(&setup.cwd).map_err(|e| {
            Die::Fatal(crate::messages::cannot_chdir(
                &setup.cwd.display().to_string(),
                &crate::global::strerror(&e),
            ))
        })?;
    }
    if mode == Mode::Required {
        setup.trace();
    }
    Ok(setup)
}

/// The configuration of the repository set up by [`setup`] (system,
/// global, local, worktree and command line). Errors are fatal like in git.
pub fn load_config(ctx: &Context, setup: &Setup) -> Result<rgit_config::ConfigSet, Die> {
    use rgit_config::sources::{self, RepoConfig, ScopeFilter};
    use rgit_config::{ConfigSet, LoadOptions, StaticRepoFacts};

    let env = SystemEnv::default();
    let git_dir = setup.absolute_git_dir();
    let branch = std::fs::read(git_dir.join("HEAD")).ok().and_then(|h| {
        h.strip_prefix(b"ref: refs/heads/")
            .map(|r| r.trim_ascii_end().into())
    });
    let facts = StaticRepoFacts {
        git_dir: Some(git_dir.clone()),
        worktree: setup.work_tree.as_ref().map(|w| setup.cwd.join(w)),
        branch,
    };
    let opts = LoadOptions {
        env: &env,
        repo: Some(&facts),
        includes: true,
    };
    let repo = RepoConfig {
        common_dir: setup.absolute_common_dir(),
        git_dir,
        worktree_config: None,
    };
    let mut set = ConfigSet::new();
    sources::load_standard(
        opts,
        Some(&repo),
        &command_line(ctx),
        ScopeFilter::default(),
        &mut set,
    )
    .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(set)
}

/// Print a setup error like git (all lines, last one `fatal:`) and exit 128.
pub fn die(e: Error) -> Die {
    let mut err = std::io::stderr();
    for (level, msg) in e.report() {
        let _ = writeln!(err, "{} {msg}", level.prefix());
    }
    Die::Exit(rgit_options::EXIT_FATAL)
}
