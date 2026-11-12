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
    // A required setup returns a repository or an error.
    setup_inner(ctx, mode == Mode::Required, true)?.ok_or(Die::Exit(rgit_options::EXIT_FATAL))
}

/// Like [`setup`], for commands that also work outside a repository
/// (`RUN_SETUP_GENTLY`): `None` when there is none.
pub fn setup_gently(ctx: &Context) -> Result<Option<Setup>, Die> {
    setup_inner(ctx, true, false)
}

fn setup_inner(ctx: &Context, trace: bool, required: bool) -> Result<Option<Setup>, Die> {
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
    let setup = if required {
        rgit_repo::setup(&opts, &mut protected).map_err(die)?
    } else {
        match rgit_repo::discover(&opts, &mut protected).map_err(die)? {
            Ok(s) => s,
            Err(_) => return Ok(None),
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

/// Print a setup error like git (all lines, last one `fatal:`) and exit 128.
pub fn die(e: Error) -> Die {
    let mut err = std::io::stderr();
    for (level, msg) in e.report() {
        let _ = writeln!(err, "{} {msg}", level.prefix());
    }
    Die::Exit(rgit_options::EXIT_FATAL)
}
