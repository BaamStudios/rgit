// SPDX-License-Identifier: MIT
//! Repository setup for commands: runs [`rgit_repo::setup`] with the
//! environment and global options, prints warnings and errors the way git
//! does, and moves into the directory the command should run in.

use std::io::Write;

use rgit_config::{CommandLine, LoadOptions, Loader, SourceSpec, SystemEnv};
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
///
/// The entries are already split into key and value (`-c` at the first `=`,
/// `--config-env` at the last), so they are taken over as they are: splitting
/// `key=value` again would break keys that contain `=`.
pub fn command_line(ctx: &Context) -> CommandLine {
    CommandLine {
        parameters: ctx
            .config_parameters
            .iter()
            .map(|(k, v)| rgit_config::cmdline::Parameter {
                key: k.as_str().into(),
                value: v.as_deref().map(Into::into),
            })
            .collect(),
    }
}

/// Read the `-c` / `--config-env` / `GIT_CONFIG_*` settings the way git's
/// first configuration read does: an invalid entry (no section, empty key,
/// relative include) is fatal with git's two-line report.
pub fn check_command_line(ctx: &Context) -> Result<(), Die> {
    let env = SystemEnv::default();
    let mut loader = Loader::new(LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    });
    loader.add(SourceSpec::CommandLine(command_line(ctx)));
    match loader.load() {
        Ok(_) => Ok(()),
        Err(e) => {
            let mut err = std::io::stderr();
            for (level, msg) in e.report() {
                let _ = writeln!(err, "{} {msg}", level.prefix());
            }
            Err(Die::Exit(rgit_options::EXIT_FATAL))
        }
    }
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
    // A required setup returns a repository or an error.
    setup_inner(ctx, support, mode == Mode::Required, true)?
        .ok_or(Die::Exit(rgit_options::EXIT_FATAL))
}

/// Like [`setup`], for commands that also work outside a repository
/// (`RUN_SETUP_GENTLY`): `None` when there is none.
pub fn setup_gently(ctx: &Context) -> Result<Option<Setup>, Die> {
    setup_inner(ctx, rgit_repo::format::Support::default(), true, false)
}

fn setup_inner(
    ctx: &Context,
    support: rgit_repo::format::Support,
    trace: bool,
    required: bool,
) -> Result<Option<Setup>, Die> {
    check_command_line(ctx)?;
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
    let setup = if required {
        rgit_repo::setup(&opts, &mut protected).map_err(die)?
    } else {
        match rgit_repo::discover(&opts, &mut protected) {
            // Like git's gentle setup: a repository of an unknown format
            // is ignored with a warning.
            Err(
                e @ (Error::VersionTooNew(_)
                | Error::UnknownExtensions(_)
                | Error::V1OnlyExtensions(_)),
            ) => {
                let dir = ignored_git_dir(&opts);
                let mut err = std::io::stderr();
                let _ = writeln!(err, "warning: {e}");
                let _ = writeln!(
                    err,
                    "warning: {}",
                    crate::messages::ignoring_git_dir(&dir, &e.to_string())
                );
                return Ok(None);
            }
            Err(e) => return Err(die(e)),
            Ok(Ok(s)) => s,
            Ok(Err(_)) => return Ok(None),
        }
    };
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
    if trace {
        setup.trace();
    }
    Ok(Some(setup))
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

/// The git directory a gentle setup ignores, as git shows it: `.git` in
/// the current directory, else the path found above it.
fn ignored_git_dir(opts: &SetupOptions) -> String {
    if let Some(d) = &opts.git_dir {
        return d.display().to_string();
    }
    let mut dir = opts.cwd.as_path();
    loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            return if dir == opts.cwd {
                ".git".to_string()
            } else {
                candidate.display().to_string()
            };
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return ".git".to_string(),
        }
    }
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
