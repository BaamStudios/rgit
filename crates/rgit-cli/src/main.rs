// SPDX-License-Identifier: MIT
//! The `rgit` binary: global options, command dispatch, exit codes.
//!
//! Behaves as `git` when installed under that name; when invoked as
//! `git-<cmd>` (e.g. `git-upload-pack`) it runs `<cmd>` directly.

mod commands;
mod context;
mod global;
mod history;
mod messages;
mod objects;
mod repo;
mod revisions;

use std::io::Write;

pub use context::Context;

/// Re-exports for command files: `use crate::prelude::*;`.
#[allow(unused_imports)]
pub mod prelude {
    pub use crate::context::Context;
    pub use crate::{CmdResult, Die};
    pub use rgit_options::{CallbackError, Opt, ParseFlags, Parser};
    pub use std::cell::{Cell, RefCell};
}

/// How a command ends abnormally.
#[derive(Debug)]
pub enum Die {
    /// `fatal: <msg>`, exit 128.
    Fatal(String),
    /// `error: <msg>`, with the given exit code.
    Error(String, i32),
    /// Pre-rendered output and exit code (help, usage errors from the
    /// option parser).
    Output(rgit_options::Exit),
    /// Exit silently with this code (the message was already printed).
    Exit(i32),
}

impl From<rgit_options::Exit> for Die {
    fn from(e: rgit_options::Exit) -> Self {
        Die::Output(e)
    }
}

impl Die {
    /// Print the message and return the exit code.
    pub fn emit(self) -> i32 {
        match self {
            Die::Fatal(m) => {
                let _ = writeln!(std::io::stderr(), "fatal: {m}");
                rgit_options::EXIT_FATAL
            }
            Die::Error(m, code) => {
                let _ = writeln!(std::io::stderr(), "error: {m}");
                code
            }
            Die::Output(e) => e.emit(),
            Die::Exit(code) => code,
        }
    }
}

/// Result of a command: the exit code, or how it died.
pub type CmdResult = Result<i32, Die>;

/// Restore the default SIGPIPE action. Rust ignores SIGPIPE, which turns
/// `rgit log | head` into "Broken pipe" errors; git dies quietly instead.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: called first thing in main, before any other thread exists;
    // setting a signal disposition to SIG_DFL has no memory-safety impact.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {}

fn program_name(argv0: &str) -> &str {
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    base.strip_suffix(".exe").unwrap_or(base)
}

fn print_main_help(to_stdout: bool) {
    let names: Vec<&str> = commands::COMMANDS.iter().map(|c| c.name).collect();
    let text = format!("{}{}", messages::GIT_USAGE, messages::help_fallback(&names));
    if to_stdout {
        let _ = std::io::stdout().write_all(text.as_bytes());
    } else {
        let _ = std::io::stderr().write_all(text.as_bytes());
    }
}

/// Run a command; `argv[0]` is its name.
fn dispatch(ctx: &mut Context, mut argv: Vec<String>) -> CmdResult {
    // `git <cmd> --help` is `git help <cmd>`.
    if argv.get(1).map(String::as_str) == Some("--help") && commands::find("help").is_some() {
        argv = vec!["help".into(), argv[0].clone()];
    }
    let name = argv[0].clone();
    if name == "help" && commands::find("help").is_none() {
        // Until `help` exists (#179): usage for `git help`, the command's
        // short help for `git help <cmd>`.
        return match argv.get(1) {
            Some(cmd) if commands::find(cmd).is_some() => {
                dispatch(ctx, vec![cmd.clone(), "-h".into()])
            }
            Some(cmd) if !cmd.starts_with('-') => {
                eprintln!("{}", messages::not_a_git_command(cmd));
                Ok(1)
            }
            _ => {
                print_main_help(true);
                Ok(0)
            }
        };
    }
    match commands::find(&name) {
        Some(cmd) => (cmd.run)(ctx, &argv),
        None => {
            eprintln!("{}", messages::not_a_git_command(&name));
            Ok(1)
        }
    }
}

fn run(args: Vec<String>) -> CmdResult {
    let argv0 = args.first().cloned().unwrap_or_else(|| "rgit".into());
    let prog = program_name(&argv0).to_string();
    let mut ctx = Context {
        invoked_as_git: prog == "git" || prog.starts_with("git-"),
        exec_path: context::default_exec_path(&argv0),
        ..Default::default()
    };
    if let Some(cmd) = prog.strip_prefix("git-") {
        let mut argv = vec![cmd.to_string()];
        argv.extend(args.into_iter().skip(1));
        return dispatch(&mut ctx, argv);
    }
    match global::parse(&mut ctx, &args[1.min(args.len())..])? {
        global::Action::Exit(code) => Ok(code),
        global::Action::NoCommand => {
            print_main_help(true);
            Ok(1)
        }
        global::Action::Run(argv) => dispatch(&mut ctx, argv),
    }
}

fn main() {
    reset_sigpipe();
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = match run(args) {
        Ok(code) => code,
        Err(die) => die.emit(),
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
