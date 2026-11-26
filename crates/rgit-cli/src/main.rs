// SPDX-License-Identifier: MIT
//! The `rgit` binary: global options, command dispatch, exit codes.
//!
//! Behaves as `git` when installed under that name; when invoked as
//! `git-<cmd>` (e.g. `git-upload-pack`) it runs `<cmd>` directly.

/// mimalloc instead of the system allocator (#596); feature `mimalloc`,
/// on by default.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Replaces std's `eprintln!` in this crate (textual scope shadows the
/// prelude). std writes the formatted pieces and the newline to the
/// unbuffered stderr one `write` at a time, so a child that shares our
/// stderr (`upload-pack` over `file://`, hooks, filters) can land in the
/// middle of a line. Like git, each message goes out in a single write.
/// A failed write is ignored, as git does, instead of panicking.
macro_rules! eprintln {
    () => {
        $crate::write_stderr("\n")
    };
    ($($arg:tt)*) => {{
        let mut line = ::std::format!($($arg)*);
        line.push('\n');
        $crate::write_stderr(&line)
    }};
}

/// Writes `s` to stderr with one `write_all` (see `eprintln!` above).
fn write_stderr(s: &str) {
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

mod abbrev;
mod alias;
mod argv;
mod attributes;
mod autocorrect;
mod cmdlist;
mod color;
mod column;
mod commands;
mod comment;
mod compare_branches;
mod context;
mod convert;
mod diff;
mod external;
mod format;
mod global;
mod grep;
mod history;
mod hooks;
mod housekeeping;
mod index_config;
mod interactive;
mod maintenance;
mod messages;
mod objects;
mod pager;
mod patch_id;
mod push;
mod quote;
mod rebase;
mod ref_filter;
mod reflog_expire;
mod refs;
mod refs_support;
mod repo;
mod repo_objects;
mod repository;
mod revision;
mod revisions;
mod sequencer;
mod signing;
mod submodules;
mod switch_branch;
mod telemetry;
mod tracking;
mod tree_changes;
mod untracked;
mod worktrees;
mod wt_status;

use std::io::Write;

use rgit_messages::{Level, prefixed};

pub use context::Context;

// The option parser keeps its own copies of the exit codes.
const _: () = assert!(rgit_options::EXIT_FATAL == rgit_messages::EXIT_FATAL);
const _: () = assert!(rgit_options::EXIT_USAGE == rgit_messages::EXIT_USAGE);

/// Re-exports for command files: `use crate::prelude::*;`.
#[allow(unused_imports)]
pub mod prelude {
    pub use crate::context::Context;
    pub use crate::{CmdResult, Die};
    pub use rgit_messages::Advice;
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
                rgit_trace::trace2::error(&m);
                eprintln!("{}", prefixed(Level::Fatal, &m));
                rgit_messages::EXIT_FATAL
            }
            Die::Error(m, code) => {
                rgit_trace::trace2::error(&m);
                eprintln!("{}", prefixed(Level::Error, &m));
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

fn print_main_help() {
    let mut text = String::new();
    commands::help::common_help(&mut text);
    let _ = std::io::stdout().write_all(text.as_bytes());
}

/// Run a command; `argv[0]` is its name: a built-in, else an external
/// `git-<name>` program, else an alias, else an unknown command.
fn dispatch(ctx: &mut Context, mut argv: Vec<String>) -> CmdResult {
    // `git <cmd> --help` is `git help <cmd>`, for commands only.
    if argv.get(1).map(String::as_str) == Some("--help") {
        argv = vec!["help".into(), "--exclude-guides".into(), argv[0].clone()];
    }
    // An alias may shadow a deprecated built-in.
    if cmdlist::DEPRECATED.contains(&argv[0].as_str()) {
        let early = alias::EarlyConfig::load(ctx)?;
        if alias::lookup(&early.config, &argv[0])?.is_some() {
            return expand_alias(ctx, early, argv);
        }
    }
    if let Some(result) = run_command(ctx, &argv) {
        return result;
    }
    let early = alias::EarlyConfig::load(ctx)?;
    if alias::lookup(&early.config, &argv[0])?.is_some() {
        return expand_alias(ctx, early, argv);
    }
    match autocorrect::unknown(ctx, &early, &argv[0]) {
        Ok(Some(corrected)) => {
            argv[0] = corrected;
            dispatch(ctx, argv)
        }
        Ok(None) => Ok(1),
        Err(result) => result,
    }
}

/// Start the pager for `cmd` when `--paginate` or `pager.<cmd>` asks for
/// it (only possible when stdout is a terminal).
fn start_pager(ctx: &Context, cmd: &str) -> Result<(), Die> {
    pager::setup(ctx, cmd, false)
}

/// Run `argv` as a built-in or external command; `None` if it is neither.
fn run_command(ctx: &mut Context, argv: &[String]) -> Option<CmdResult> {
    let name = &argv[0];
    let builtin = commands::find(name);
    if builtin.is_none() && external::find(ctx, name).is_none() {
        // Still trace the attempt, as git does.
        return external::run(ctx, argv);
    }
    ctx.command.clone_from(name);
    match builtin {
        Some(_) => telemetry::cmd_name(ctx, name, true),
        None => telemetry::cmd_name(ctx, "_run_dashed_", false),
    }
    // Commands that decide about the pager themselves (some of their modes
    // page by default); `--paginate` pages them at once.
    let own_pager = matches!(
        name.as_str(),
        "log" | "diff" | "branch" | "config" | "reflog"
    ) && ctx.paginate != Some(true);
    // `pager.version` is ignored.
    if !own_pager && (name != "version" || ctx.paginate == Some(true)) {
        if let Err(e) = start_pager(ctx, name) {
            return Some(Err(e));
        }
    }
    if let Some(cmd) = builtin {
        rgit_trace::trace_builtin(argv);
        // Computing it reads the configuration (`pager.<cmd>`); only the
        // few commands that run a filter need it.
        let child = ctx.clone();
        let pager_started = pager::started();
        rgit_convert::filter::set_base_env_with(move || child.child_env_with(pager_started));
        return Some((cmd.run)(ctx, argv));
    }
    external::run(ctx, argv)
}

/// Expand the alias `argv[0]` (and the aliases it expands to) and run the
/// result.
fn expand_alias(
    ctx: &mut Context,
    mut early: alias::EarlyConfig,
    mut argv: Vec<String>,
) -> CmdResult {
    let first = argv[0].clone();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let name = argv[0].clone();
        let Some(value) = alias::lookup(&early.config, &name)? else {
            eprintln!("{}", messages::help::alias_expansion_failed(&first, &name));
            return Ok(1);
        };
        if let Some(pos) = seen.iter().position(|s| *s == name) {
            return Err(Die::Fatal(alias_loop(&first, &seen, pos)));
        }
        seen.push(name.clone());
        if argv.len() == 2 && argv[1] == "-h" {
            eprintln!("{}", messages::help::is_aliased_to(&name, &value));
        }
        if rgit_trace::trace2::enabled() {
            telemetry::cmd_name(ctx, "_run_dashed_", false);
            match value.strip_prefix('!') {
                Some(cmd) => {
                    let mut words = vec![cmd.to_string()];
                    words.extend(argv[1..].iter().cloned());
                    rgit_trace::trace2::alias(&name, &words);
                    telemetry::cmd_name(ctx, "_run_shell_alias_", false);
                }
                None => {
                    if let Ok(words) = alias::split(&value) {
                        rgit_trace::trace2::alias(&name, &words);
                    }
                    telemetry::cmd_name(ctx, "_run_git_alias_", false);
                }
            }
        }
        if let Some(cmd) = value.strip_prefix('!') {
            return alias::run_shell(ctx, &early, &name, cmd, &argv[1..]);
        }
        let words = alias::split(&value)
            .map_err(|reason| Die::Fatal(messages::help::bad_alias_string(&name, reason)))?;
        rgit_trace::trace_alias_expansion(&name, &words);
        let before = ctx.config_parameters.len();
        let words = match alias::global_options(ctx, &name, words)? {
            Ok(w) => w,
            Err(code) => return Ok(code),
        };
        if words[0] == name {
            return Err(Die::Fatal(messages::help::recursive_alias(&name)));
        }
        argv.splice(0..1, words);
        if commands::find(&argv[0]).is_some() {
            // git runs the expanded built-in as a child `git`; rgit runs it
            // in this process but traces the same steps.
            let mut child = vec!["git".to_owned()];
            child.extend(argv.iter().cloned());
            rgit_trace::trace_exec(&child);
            rgit_trace::trace_run_command::<String>(&[], &child);
            child[0] = ctx.exec_path.join("git").to_string_lossy().into_owned();
            rgit_trace::trace_start_command(&child);
        }
        if let Some(result) = run_command(ctx, &argv) {
            return result;
        }
        if ctx.config_parameters.len() != before {
            early = alias::EarlyConfig::load(ctx)?;
        }
    }
}

/// The report of an alias loop: the chain, the repeated alias marked.
fn alias_loop(first: &str, seen: &[String], repeated: usize) -> String {
    let mut m = messages::help::alias_loop(first);
    for (i, s) in seen.iter().enumerate() {
        m.push_str("\n  ");
        m.push_str(s);
        if i == repeated {
            m.push_str(" <==");
        }
        if i + 1 == seen.len() {
            m.push_str(" ==>");
        }
    }
    m
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
        // A dashed name runs the built-in; there is nothing else to look
        // up (a `git-<cmd>` link to rgit would find itself again).
        if commands::find(cmd).is_none() {
            return Err(Die::Fatal(messages::help::cannot_handle_as_builtin(cmd)));
        }
        let mut argv = vec![cmd.to_string()];
        argv.extend(args.into_iter().skip(1));
        return dispatch(&mut ctx, argv);
    }
    let parsed = global::parse(&mut ctx, &args[1.min(args.len())..]);
    if rgit_trace::trace2::enabled() && matches!(parsed, Ok(global::Action::Exit(_)) | Err(_)) {
        let name = if matches!(parsed, Ok(global::Action::Exit(0))) {
            "_query_"
        } else {
            "_usage_"
        };
        telemetry::cmd_name(&ctx, name, false);
    }
    match parsed? {
        global::Action::Exit(code) => Ok(code),
        global::Action::NoCommand => {
            if ctx.paginate == Some(true) {
                start_pager(&ctx, "")?;
            }
            print_main_help();
            Ok(1)
        }
        global::Action::Run(argv) => dispatch(&mut ctx, argv),
    }
}

fn main() {
    let start = std::time::Instant::now();
    reset_sigpipe();
    let args: Vec<String> = std::env::args_os().map(argv::to_string).collect();
    telemetry::init(&args);
    // The command runs on a thread with a stack of its own: scripts start
    // git under a small stack limit (t5551/t5559 clone under
    // `ulimit -s 128`), which the main thread inherits.
    let cmd_args = args.clone();
    let code = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || match run(cmd_args) {
            Ok(code) => code,
            Err(die) => die.emit(),
        })
        .map(|t| t.join().unwrap_or(101))
        .unwrap_or_else(|_| match run(args.clone()) {
            Ok(code) => code,
            Err(die) => die.emit(),
        });
    rgit_trace::trace2::exit(code);
    // Filter processes see the end of their input and finish.
    rgit_convert::filter::finish_all();
    let _ = std::io::stdout().flush();
    // GIT_TRACE_PERFORMANCE: the whole command line as invoked.
    rgit_trace::trace_command_performance(start, &args);
    pager::finish();
    rgit_trace::trace2::atexit();
    std::process::exit(code);
}
