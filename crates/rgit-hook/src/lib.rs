// SPDX-License-Identifier: MIT
//! Finding and running hooks (Documentation/githooks.adoc,
//! Documentation/git-hook.adoc, Documentation/config/hook.adoc,
//! `core.hooksPath` in Documentation/config/core.adoc).
//!
//! A hook event runs the hooks configured with `hook.<name>.event` in the
//! order of their configuration lines, then the executable file named
//! after the event in the hook directory (`$GIT_DIR/hooks` or
//! `core.hooksPath`). A file without the executable bit is skipped with the
//! `advice.ignoredHook` hint.
//!
//! Hooks run in the given directory (the top of the work tree, or the git
//! directory of a bare repository) with the caller's environment. Their
//! standard output goes to standard error unless the caller keeps it apart
//! (`pre-push`); standard input is empty, a file or bytes fed through a
//! pipe. The exit codes of all hooks of an event are or-ed together.
//!
//! With more than one job (`-j`, `hook.<event>.jobs`, `hook.jobs`; the
//! configured values only when every configured hook has
//! `hook.<name>.parallel`), the output of each hook is collected and
//! written to standard error in one piece; events that work on shared
//! state still run one hook at a time.

mod config;
pub mod messages;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

use rgit_config::{ConfigSet, Scope};
use rgit_messages::{Advice, Advisor};

use config::HookConfig;
use messages as msg;

/// Every hook event `githooks(5)` describes.
pub const KNOWN_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

/// Events whose hooks never run at the same time (`hook.jobs` in
/// Documentation/config/hook.adoc).
pub const SERIAL_ONLY: &[&str] = &[
    "applypatch-msg",
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-checkout",
    "push-to-checkout",
];

/// Whether `name` is a hook event `githooks(5)` describes.
pub fn is_known(name: &str) -> bool {
    KNOWN_HOOKS.contains(&name)
}

fn warn(text: &str) {
    let _ = writeln!(std::io::stderr(), "warning: {text}");
}

fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// The hook directory.
#[derive(Debug, Clone)]
pub struct HookDir {
    /// As git names it in messages and `$0`: relative to the directory the
    /// hooks run in, or absolute.
    pub shown: PathBuf,
    /// Absolute.
    pub path: PathBuf,
}

/// Where a hook comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `hook.<name>.command`.
    Configured { name: String, command: String },
    /// A file in the hook directory.
    HookDir { shown: PathBuf, path: PathBuf },
}

/// A hook that will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub source: Source,
    /// `hook.<name>.parallel`.
    pub parallel: bool,
}

/// State of a hook in `git hook list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Enabled,
    /// `hook.<name>.enabled = false`.
    Disabled,
    /// `hook.<event>.enabled = false`.
    EventDisabled,
}

/// One line of `git hook list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The friendly name; `None` for the hook directory.
    pub name: Option<String>,
    /// Scope of the configuration that defines the hook.
    pub scope: Option<Scope>,
    pub state: State,
}

/// Standard input of the hooks.
#[derive(Debug, Clone, Copy)]
pub enum Input<'a> {
    /// `/dev/null`.
    Null,
    /// These bytes, through a pipe (each hook gets all of them).
    Data(&'a [u8]),
    /// This file, from the start for each hook.
    File(&'a Path),
}

/// How to run the hooks of an event.
#[derive(Debug, Clone)]
pub struct RunOptions<'a> {
    pub args: Vec<OsString>,
    pub stdin: Input<'a>,
    /// Keep the hooks' standard output apart from standard error
    /// (`pre-push`), as long as they run one at a time.
    pub separate_stdout: bool,
    /// `-j`: the job count (-1: CPU count); `None` uses the configuration.
    pub jobs: Option<i64>,
    /// Extra environment variables for this event.
    pub env: Vec<(OsString, OsString)>,
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        RunOptions {
            args: Vec::new(),
            stdin: Input::Null,
            separate_stdout: false,
            jobs: None,
            env: Vec::new(),
        }
    }
}

impl<'a> RunOptions<'a> {
    /// Options with these arguments.
    pub fn with_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        RunOptions {
            args: args.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }

    /// Add an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Set the standard input.
    #[must_use]
    pub fn stdin(mut self, input: Input<'a>) -> Self {
        self.stdin = input;
        self
    }
}

/// What running an event did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Whether any hook is configured or present for the event, even one
    /// that is disabled (`git hook run` reports a missing hook otherwise).
    pub found: bool,
    /// Number of hooks run.
    pub ran: usize,
    /// The or-ed exit codes (0: all succeeded or none ran).
    pub code: i32,
}

impl Outcome {
    /// Whether every hook succeeded (or none ran).
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// The hooks of a repository (or, outside one, the configured hooks).
pub struct Hooks {
    config: HookConfig,
    dir: Option<HookDir>,
    run_dir: PathBuf,
    env: Vec<(OsString, OsString)>,
    advisor: Advisor,
    warned: std::cell::Cell<bool>,
}

impl Hooks {
    /// Hooks configured in `config`, the hook directory `dir`, run in
    /// `run_dir` (absolute) with the extra environment `env`.
    pub fn new(
        config: &ConfigSet,
        dir: Option<HookDir>,
        run_dir: PathBuf,
        env: Vec<(OsString, OsString)>,
        advisor: Advisor,
    ) -> Hooks {
        Hooks {
            config: HookConfig::read(config),
            dir,
            run_dir,
            env,
            advisor,
            warned: std::cell::Cell::new(false),
        }
    }

    /// The directory hooks run in.
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Fatal configuration errors, and the configuration warnings (once).
    fn check(&self) -> Result<(), String> {
        self.config.check()?;
        if !self.warned.replace(true) {
            self.config.warn_misplaced_jobs();
        }
        Ok(())
    }

    /// The hook file for `event` in the hook directory, if it exists:
    /// `(shown, absolute, executable)`.
    fn hookdir_file(&self, event: &str) -> Option<(PathBuf, PathBuf, bool)> {
        let dir = self.dir.as_ref()?;
        let path = dir.path.join(event);
        std::fs::metadata(&path).ok()?;
        let exec = is_executable(&path);
        Some((dir.shown.join(event), path, exec))
    }

    fn advise_ignored(&self, shown: &Path) {
        if self.advisor.enabled(Advice::IgnoredHook).unwrap_or(false) {
            let text = msg::ignored_hook(&shown.to_string_lossy());
            let _ = std::io::stderr().write_all(self.advisor.hint(&text).as_bytes());
        }
    }

    /// The hooks that run for `event`, in order. Prints the
    /// `advice.ignoredHook` hint for a hook file that is not executable.
    pub fn find(&self, event: &str) -> Result<Vec<Hook>, String> {
        self.check()?;
        let mut out = Vec::new();
        let event_off = self.config.event_disabled(event);
        for name in self.config.events.get(event).into_iter().flatten() {
            let n = &self.config.named[name];
            if n.enabled == Some(false) {
                if n.command.is_none() {
                    warn(&msg::disabled_without_command(name));
                }
                continue;
            }
            if event_off {
                continue;
            }
            if name == event && !is_known(event) {
                warn(&msg::same_as_event(name));
            }
            let Some((command, _)) = &n.command else {
                continue;
            };
            out.push(Hook {
                source: Source::Configured {
                    name: name.clone(),
                    command: command.clone(),
                },
                parallel: n.parallel == Some(true),
            });
        }
        if let Some((shown, path, exec)) = self.hookdir_file(event) {
            if exec {
                out.push(Hook {
                    source: Source::HookDir { shown, path },
                    parallel: true,
                });
            } else {
                self.advise_ignored(&shown);
            }
        }
        Ok(out)
    }

    /// Whether a hook would run for `event` (no messages).
    pub fn exists(&self, event: &str) -> bool {
        let configured = !self.config.event_disabled(event)
            && self
                .config
                .events
                .get(event)
                .into_iter()
                .flatten()
                .any(|name| {
                    let n = &self.config.named[name];
                    n.enabled != Some(false) && n.command.is_some()
                });
        configured || self.hookdir_file(event).is_some_and(|(_, _, x)| x)
    }

    /// The lines of `git hook list <event>`.
    pub fn list(&self, event: &str) -> Result<Vec<Listed>, String> {
        self.check()?;
        let mut out = Vec::new();
        let event_off = self.config.event_disabled(event);
        for name in self.config.events.get(event).into_iter().flatten() {
            let n = &self.config.named[name];
            if n.enabled == Some(false) && n.command.is_none() {
                warn(&msg::disabled_without_command(name));
            }
            let state = if event_off {
                State::EventDisabled
            } else if n.enabled == Some(false) {
                State::Disabled
            } else {
                State::Enabled
            };
            out.push(Listed {
                name: Some(name.clone()),
                scope: n.command.as_ref().map(|c| c.1).or(n.event_scope),
                state,
            });
        }
        if let Some((shown, _, exec)) = self.hookdir_file(event) {
            if exec {
                out.push(Listed {
                    name: None,
                    scope: None,
                    state: State::Enabled,
                });
            } else {
                self.advise_ignored(&shown);
            }
        }
        Ok(out)
    }

    /// Runs the hooks of `event`. `Err` is a fatal error (configuration,
    /// `-j` value).
    pub fn run(&self, event: &str, opts: &RunOptions<'_>) -> Result<Outcome, String> {
        if let Some(j) = opts.jobs {
            if j < -1 {
                return Err(msg::invalid_jobs_flag(j));
            }
        }
        // Like git, the job settings are checked (and warned about) even
        // when no hook runs.
        let configured_jobs = self.config.jobs_for(event);
        let hooks = self.find(event)?;
        if hooks.is_empty() {
            let found = self.config.events.get(event).is_some_and(|l| !l.is_empty());
            return Ok(Outcome {
                found,
                ran: 0,
                code: 0,
            });
        }
        let configured = || {
            hooks.iter().filter_map(|h| match &h.source {
                Source::Configured { name, .. } => Some((name, h.parallel)),
                Source::HookDir { .. } => None,
            })
        };
        let jobs = match opts.jobs.filter(|&j| j != 0) {
            Some(j) => {
                let n = if j == -1 { cpus() } else { j as usize };
                if n > 1 {
                    // Every configured hook of the event, enabled or not.
                    for name in self.config.events.get(event).into_iter().flatten() {
                        if self.config.named[name].parallel != Some(true) {
                            warn(&msg::not_marked_parallel(name, &j.to_string()));
                        }
                    }
                }
                n
            }
            None => match configured_jobs {
                Some(n) if configured().all(|(_, p)| p) => n,
                _ => 1,
            },
        };
        let piped = jobs > 1;
        let width = if SERIAL_ONLY.contains(&event) {
            1
        } else {
            jobs
        };
        let _ = std::io::stdout().flush();
        let _sigpipe = SigpipeIgnored::new();
        let mut code = 0;
        let mut running: VecDeque<Running> = VecDeque::new();
        for hook in &hooks {
            while running.len() >= width {
                if let Some(r) = running.pop_front() {
                    code |= r.finish();
                }
            }
            match self.start(hook, opts, piped) {
                Ok(r) => running.push_back(r),
                Err(c) => code |= c,
            }
        }
        while let Some(r) = running.pop_front() {
            code |= r.finish();
        }
        Ok(Outcome {
            found: true,
            ran: hooks.len(),
            code,
        })
    }

    fn start(&self, hook: &Hook, opts: &RunOptions<'_>, piped: bool) -> Result<Running, i32> {
        let (mut cmd, shown) = match &hook.source {
            Source::Configured { command, .. } => {
                let args: Vec<&std::ffi::OsStr> =
                    opts.args.iter().map(OsString::as_os_str).collect();
                (
                    rgit_ident::editor::shell_command(command, &args),
                    command.clone(),
                )
            }
            Source::HookDir { shown, path } => {
                // A relative name is relative to the directory the hook
                // runs in; scripts see it as `$0`.
                let mut c = Command::new(if shown.is_absolute() { path } else { shown });
                c.args(&opts.args);
                (c, shown.to_string_lossy().into_owned())
            }
        };
        cmd.current_dir(&self.run_dir);
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd.envs(opts.env.iter().map(|(k, v)| (k, v)));
        let data = match opts.stdin {
            Input::Null => {
                cmd.stdin(Stdio::null());
                None
            }
            Input::Data(d) => {
                cmd.stdin(Stdio::piped());
                Some(d.to_vec())
            }
            Input::File(p) => {
                match std::fs::File::open(p) {
                    Ok(f) => cmd.stdin(f),
                    Err(_) => cmd.stdin(Stdio::null()),
                };
                None
            }
        };
        let mut reader = None;
        if piped {
            match std::io::pipe() {
                Ok((r, w)) => {
                    if let Ok(w2) = w.try_clone() {
                        cmd.stdout(w2);
                    }
                    cmd.stderr(w);
                    reader = Some(r);
                }
                Err(_) => {
                    cmd.stdout(stderr_stdio());
                }
            }
        } else if !opts.separate_stdout {
            cmd.stdout(stderr_stdio());
        }
        let spawned = cmd.spawn();
        // The command holds the write end of the output pipe.
        drop(cmd);
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "{}",
                    msg::cannot_exec(&shown, &strerror(&e))
                );
                return Err(1);
            }
        };
        let feeder = match (data, child.stdin.take()) {
            (Some(d), Some(mut w)) => Some(std::thread::spawn(move || {
                let _ = w.write_all(&d);
            })),
            _ => None,
        };
        let output = reader.map(|mut r| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                buf
            })
        });
        Ok(Running {
            child,
            shown,
            feeder,
            output,
        })
    }
}

/// A started hook.
struct Running {
    child: Child,
    shown: String,
    feeder: Option<JoinHandle<()>>,
    output: Option<JoinHandle<Vec<u8>>>,
}

impl Running {
    /// Waits for the hook and returns its exit code.
    fn finish(mut self) -> i32 {
        let status = self.child.wait();
        if let Some(f) = self.feeder.take() {
            let _ = f.join();
        }
        if let Some(o) = self.output.take() {
            if let Ok(buf) = o.join() {
                let _ = std::io::stderr().write_all(&buf);
            }
        }
        match status {
            Ok(s) => match s.code() {
                Some(c) => c,
                None => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        if let Some(sig) = s.signal() {
                            let _ = writeln!(
                                std::io::stderr(),
                                "{}",
                                msg::died_of_signal(&self.shown, sig)
                            );
                            return 128 + sig;
                        }
                    }
                    1
                }
            },
            Err(_) => 1,
        }
    }
}

/// Standard error of this process as a child's standard output.
fn stderr_stdio() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        if let Ok(fd) = std::io::stderr().as_fd().try_clone_to_owned() {
            return Stdio::from(fd);
        }
    }
    Stdio::inherit()
}

/// The system's description of an I/O error, without Rust's
/// " (os error N)" suffix.
fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated string that outlives the call.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Ignores SIGPIPE while hooks run, so that a hook that does not read its
/// input cannot kill us while we write it; restores the old action on drop.
struct SigpipeIgnored {
    #[cfg(unix)]
    old: libc::sighandler_t,
}

impl SigpipeIgnored {
    fn new() -> Self {
        #[cfg(unix)]
        {
            // SAFETY: changing the disposition of SIGPIPE has no
            // memory-safety impact; the previous value is restored on drop.
            let old = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
            SigpipeIgnored { old }
        }
        #[cfg(not(unix))]
        SigpipeIgnored {}
    }
}

impl Drop for SigpipeIgnored {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: see `SigpipeIgnored::new`.
        unsafe {
            libc::signal(libc::SIGPIPE, self.old);
        }
    }
}

#[cfg(test)]
mod tests;
