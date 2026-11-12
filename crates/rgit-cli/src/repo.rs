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
    check_command_line(ctx)?;
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

/// Print a setup error like git (all lines, last one `fatal:`) and exit 128.
pub fn die(e: Error) -> Die {
    let mut err = std::io::stderr();
    for (level, msg) in e.report() {
        let _ = writeln!(err, "{} {msg}", level.prefix());
    }
    Die::Exit(rgit_options::EXIT_FATAL)
}
