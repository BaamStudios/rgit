// SPDX-License-Identifier: MIT
//! The pager (Documentation/config/pager.adoc, `core.pager` in
//! Documentation/config/core.adoc, `--paginate` and `GIT_PAGER` in
//! Documentation/git.adoc).
//!
//! Behaviour, checked against the `git` binary:
//!
//! * The pager runs only when stdout is a terminal. Its command is
//!   `$GIT_PAGER`, then `pager.<cmd>` (when it is not a boolean and
//!   `--paginate` was not given), then `core.pager`, `$PAGER` and `less`; an empty command or `cat` means no
//!   pager. The command goes through the shell when it contains shell
//!   syntax.
//! * `LESS=FRX` and `LV=-c` are added to the pager's environment when they
//!   are unset.
//! * Our stdout (and stderr, when it is a terminal) is redirected into the
//!   pager. Commands started later see `GIT_PAGER_IN_USE=true`; after
//!   `--no-pager` or with `pager.<cmd>` false they see `GIT_PAGER=cat`.
//! * At exit both are closed and we wait for the pager; its exit status
//!   does not change ours. A signal (also SIGPIPE when the pager quit
//!   early) waits for the pager as well and then kills us with the same
//!   signal.
//! * `--paginate` pages every command with the default pager,
//!   `--no-pager` none; otherwise `pager.<cmd>` decides, then the
//!   command's default.

use std::io::{IsTerminal, Write};

use rgit_config::{ConfigSet, Environment, SystemEnv};

use crate::messages::pager as msg;
use crate::{Context, Die};

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Process id of the running pager, 0 when there is none.
    pub static PID: AtomicI32 = AtomicI32::new(0);
    /// stderr was redirected into the pager as well.
    pub static STDERR: AtomicBool = AtomicBool::new(false);

    const SIGNALS: [libc::c_int; 5] = [
        libc::SIGINT,
        libc::SIGHUP,
        libc::SIGTERM,
        libc::SIGQUIT,
        libc::SIGPIPE,
    ];

    /// Close our ends of the pipe and wait for the pager. Only uses
    /// async-signal-safe calls.
    fn close_and_wait() {
        let pid = PID.swap(0, Ordering::SeqCst);
        if pid <= 0 {
            return;
        }
        // SAFETY: closing file descriptors and waiting for our own child
        // are async-signal-safe and touch no Rust-owned memory.
        unsafe {
            libc::close(1);
            if STDERR.load(Ordering::SeqCst) {
                libc::close(2);
            }
            let mut status = 0;
            while libc::waitpid(pid, &mut status, 0) < 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
            {}
        }
    }

    extern "C" fn on_signal(sig: libc::c_int) {
        close_and_wait();
        // SAFETY: restoring the default action and re-raising the signal
        // are async-signal-safe.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    /// Redirect stdout (and stderr) into `fd`, the write end of the
    /// pager's pipe, and remember the pager.
    pub fn attach(fd: libc::c_int, pid: i32, stderr_too: bool) {
        // SAFETY: plain descriptor operations on descriptors we own; the
        // handler only uses async-signal-safe calls.
        unsafe {
            libc::dup2(fd, 1);
            if stderr_too {
                libc::dup2(fd, 2);
            }
            libc::close(fd);
        }
        STDERR.store(stderr_too, Ordering::SeqCst);
        PID.store(pid, Ordering::SeqCst);
        for sig in SIGNALS {
            // SAFETY: installs a handler that only uses async-signal-safe
            // calls.
            unsafe {
                libc::signal(sig, on_signal as *const () as libc::sighandler_t);
            }
        }
    }

    pub fn finish() {
        close_and_wait();
    }

    pub fn running() -> bool {
        PID.load(Ordering::SeqCst) > 0
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn finish() {}
    pub fn running() -> bool {
        false
    }
}

/// We started a pager.
pub fn started() -> bool {
    imp::running()
}

/// Output goes to a pager: ours, or the one of the git that started us
/// (`GIT_PAGER_IN_USE`).
pub fn in_use() -> bool {
    started()
        || std::env::var_os("GIT_PAGER_IN_USE").is_some_and(|v| {
            rgit_config::value::parse_bool(Some(v.as_encoded_bytes())).unwrap_or(false)
        })
}

/// Close the pager's input and wait for it (at exit).
pub fn finish() {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    imp::finish();
}

/// `pager.<cmd>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    Unset,
    Off,
    On,
    /// Page with this command.
    Command(String),
}

/// Read `pager.<cmd>`: a boolean, or else a pager command.
pub fn setting(config: &ConfigSet, cmd: &str) -> Setting {
    let Some(e) = config.get(format!("pager.{cmd}")) else {
        return Setting::Unset;
    };
    match e.value.as_deref() {
        None => Setting::On,
        Some(v) => match rgit_config::value::parse_bool_text(Some(v)) {
            Some(true) => Setting::On,
            Some(false) => Setting::Off,
            None => Setting::Command(String::from_utf8_lossy(v).into_owned()),
        },
    }
}

/// The configuration for choosing the pager before a command has set up
/// its repository: the repository is looked for without changing
/// directory and without reporting problems (the command does that).
pub fn config(ctx: &Context) -> ConfigSet {
    use rgit_config::sources::{self, RepoConfig, ScopeFilter};
    use rgit_repo::{SetupOptions, StandardProtectedConfig};

    let env = SystemEnv::default();
    let repo = (|| {
        let mut opts = SetupOptions::from_process().ok()?;
        if let Some(d) = &ctx.git_dir {
            opts.git_dir = Some(d.into());
        }
        if let Some(w) = &ctx.work_tree {
            opts.work_tree = Some(w.into());
        }
        let mut protected = StandardProtectedConfig::new(&env, crate::repo::command_line(ctx));
        let setup = rgit_repo::discover(&opts, &mut protected).ok()?.ok()?;
        Some(RepoConfig {
            common_dir: setup.absolute_common_dir(),
            git_dir: setup.absolute_git_dir(),
            worktree_config: None,
        })
    })();
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let mut set = ConfigSet::new();
    let _ = sources::load_standard(
        opts,
        repo.as_ref(),
        &crate::repo::command_line(ctx),
        ScopeFilter::default(),
        &mut set,
    );
    set
}

/// `--no-pager`, or `pager.<cmd>` is false for the running command: git
/// then exports `GIT_PAGER=cat` so that nothing started by it pages
/// either.
pub fn disabled(ctx: &Context, config: &ConfigSet) -> bool {
    match ctx.paginate {
        Some(false) => true,
        Some(true) => false,
        None => !ctx.command.is_empty() && setting(config, &ctx.command) == Setting::Off,
    }
}

/// [`setup_for`] with the configuration read by [`config`] (only when a
/// pager is possible at all).
pub fn setup(ctx: &Context, cmd: &str, default: bool) -> Result<(), Die> {
    if ctx.paginate == Some(false) || started() || !std::io::stdout().is_terminal() {
        return Ok(());
    }
    setup_for(ctx, &config(ctx), cmd, default)
}

/// Start the pager for `cmd` if the options and configuration ask for it;
/// `default` is whether the command pages by default.
pub fn setup_for(ctx: &Context, config: &ConfigSet, cmd: &str, default: bool) -> Result<(), Die> {
    if ctx.paginate == Some(false) || started() || !std::io::stdout().is_terminal() {
        return Ok(());
    }
    // `--paginate` uses the default pager, not `pager.<cmd>`.
    let (want, command) = match ctx.paginate {
        Some(true) => (true, None),
        _ => match setting(config, cmd) {
            Setting::Command(c) => (true, Some(c)),
            Setting::On => (true, None),
            Setting::Off => (false, None),
            Setting::Unset => (default, None),
        },
    };
    if want {
        start(ctx, config, command.as_deref())?;
    }
    Ok(())
}

/// The pager command: `GIT_PAGER`, `command` (from `pager.<cmd>`), then
/// `core.pager`, `PAGER` and `less`. `None` when nothing should run.
pub fn command(config: &ConfigSet, env: &dyn Environment, command: Option<&str>) -> Option<String> {
    let p = match (env.var("GIT_PAGER"), command) {
        (Some(p), _) => p.to_string_lossy().into_owned(),
        (None, Some(c)) => c.to_owned(),
        (None, None) => rgit_ident::editor::pager(config, env).ok()?,
    };
    (!p.is_empty() && p != "cat").then_some(p)
}

/// Start the pager (`command` from `pager.<cmd>`, else the default one)
/// when stdout is a terminal.
pub fn start(ctx: &Context, config: &ConfigSet, command: Option<&str>) -> Result<(), Die> {
    if started() || !std::io::stdout().is_terminal() {
        return Ok(());
    }
    let env = SystemEnv::default();
    let Some(pager) = self::command(config, &env, command) else {
        return Ok(());
    };
    spawn(ctx, &pager)
}

#[cfg(unix)]
fn spawn(ctx: &Context, pager: &str) -> Result<(), Die> {
    use std::os::fd::IntoRawFd;
    use std::process::Stdio;

    let _ = std::io::stdout().flush();
    let mut cmd = rgit_ident::editor::shell_command(pager, &[]);
    cmd.stdin(Stdio::piped());
    cmd.envs(ctx.child_env());
    for (k, v) in [("LESS", "FRX"), ("LV", "-c")] {
        if std::env::var_os(k).is_none() {
            cmd.env(k, v);
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "error: {}",
                msg::cannot_run(pager, &rgit_messages::strerror(&e))
            );
            return Err(Die::Fatal(msg::unable_to_execute_pager(pager)));
        }
    };
    let Some(stdin) = child.stdin.take() else {
        return Ok(());
    };
    let stderr_too = std::io::stderr().is_terminal();
    let pid = i32::try_from(child.id()).unwrap_or(0);
    imp::attach(stdin.into_raw_fd(), pid, stderr_too);
    // The pager is waited for by pid in `finish`.
    drop(child);
    Ok(())
}

#[cfg(not(unix))]
fn spawn(_ctx: &Context, _pager: &str) -> Result<(), Die> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{MapEnv, Origin, Scope, parse_bytes};

    fn set(text: &str) -> ConfigSet {
        parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Command).unwrap()
    }

    #[test]
    fn pager_setting() {
        assert_eq!(setting(&set(""), "log"), Setting::Unset);
        assert_eq!(setting(&set("[pager]\nlog\n"), "log"), Setting::On);
        assert_eq!(setting(&set("[pager]\nlog = false\n"), "log"), Setting::Off);
        assert_eq!(setting(&set("[pager]\nlog = yes\n"), "log"), Setting::On);
        assert_eq!(
            setting(&set("[pager]\nlog = less -S\n"), "log"),
            Setting::Command("less -S".into())
        );
    }

    #[test]
    fn pager_command_order() {
        let none = set("");
        let core = set("[core]\npager = most\n");
        let env = MapEnv::default();
        assert_eq!(command(&none, &env, None).as_deref(), Some("less"));
        assert_eq!(command(&core, &env, None).as_deref(), Some("most"));
        assert_eq!(command(&core, &env, Some("x")).as_deref(), Some("x"));
        let git_pager = MapEnv::new([("GIT_PAGER", "y")]);
        assert_eq!(command(&core, &git_pager, Some("x")).as_deref(), Some("y"));
        for off in ["", "cat"] {
            let e = MapEnv::new([("GIT_PAGER", off)]);
            assert_eq!(command(&core, &e, Some("x")), None);
            let e = MapEnv::new([("PAGER", off)]);
            assert_eq!(command(&none, &e, None), None);
        }
    }
}
