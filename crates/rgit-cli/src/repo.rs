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
    setup_with(ctx, mode, rgit_repo::format::Support::default())
}

/// [`setup`] for a command that can handle the repository formats in
/// `support` (e.g. SHA-256).
pub fn setup_with(
    ctx: &Context,
    mode: Mode,
    support: rgit_repo::format::Support,
) -> Result<Setup, Die> {
    let env = SystemEnv::default();
    let mut opts = SetupOptions::from_process().map_err(die)?;
    opts.support = support;
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

/// The configuration of the repository (all scopes plus `-c`), or as much
/// of it as can be read.
pub fn config(ctx: &Context, setup: &Setup) -> rgit_config::ConfigSet {
    let env = SystemEnv::default();
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let repo_cfg = rgit_config::sources::RepoConfig {
        common_dir: setup.common_dir.clone(),
        git_dir: setup.git_dir.clone(),
        worktree_config: None,
    };
    let mut set = rgit_config::ConfigSet::new();
    let _ = rgit_config::sources::load_standard(
        opts,
        Some(&repo_cfg),
        &command_line(ctx),
        rgit_config::sources::ScopeFilter::default(),
        &mut set,
    );
    set
}

/// The object database of the repository: packs, loose objects and
/// alternates ([`rgit_odb::ObjectDatabase::open_for_repo`]). This is how
/// commands read objects. Configuration warnings are printed; an invalid
/// setting is fatal, as in git. Corrupt copies skipped during lookups are
/// printed as `error: ...` by the database.
#[allow(dead_code)] // for the commands that read objects (cat-file, log, ls-tree, ...)
pub fn odb(ctx: &Context, setup: &Setup) -> Result<rgit_odb::ObjectDatabase, Die> {
    odb_with_config(setup, &config(ctx, setup))
}

/// [`odb`] with an already loaded configuration.
pub fn odb_with_config(
    setup: &Setup,
    config: &rgit_config::ConfigSet,
) -> Result<rgit_odb::ObjectDatabase, Die> {
    let mut warnings = Vec::new();
    let odb = rgit_odb::ObjectDatabase::open_for_repo(setup, config, &mut warnings)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut err = std::io::stderr();
    for w in warnings {
        let _ = writeln!(err, "warning: {w}");
    }
    Ok(odb)
}
