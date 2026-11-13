// SPDX-License-Identifier: MIT
//! The user's editor and pager, and running the editor.
//!
//! From `git-var(1)` (`GIT_EDITOR`, `GIT_SEQUENCE_EDITOR`, `GIT_PAGER`)
//! and the behaviour of the `git` binary (t0007, t7005):
//!
//! * Editor: `$GIT_EDITOR`, `core.editor`, `$VISUAL` (not on a dumb
//!   terminal), `$EDITOR`, then `vi` (not on a dumb terminal). The
//!   terminal is dumb when `TERM` is unset or `dumb`. A variable that is
//!   set but empty still counts.
//! * Sequence editor: `$GIT_SEQUENCE_EDITOR`, `sequence.editor`, then the
//!   editor.
//! * Pager: `$GIT_PAGER`, `core.pager`, `$PAGER`, then `less`; an empty
//!   value means `cat`.
//!
//! The editor command is interpreted by the shell: when it contains shell
//! syntax it runs as `sh -c '<editor> "$@"' '<editor>' <file>`, otherwise
//! directly. `:` does not run anything.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use bstr::ByteSlice;
use rgit_config::{ConfigSet, Environment, Level};

use crate::config::{self, MissingStyle, Problem};
use crate::messages as msg;

/// The fallback editor.
pub const DEFAULT_EDITOR: &str = "vi";
/// The fallback pager.
pub const DEFAULT_PAGER: &str = "less";
/// The shell used for editor and pager commands.
pub const SHELL_PATH: &str = "/bin/sh";

/// `TERM` is unset or `dumb`.
pub fn terminal_is_dumb(env: &dyn Environment) -> bool {
    match env.var("TERM") {
        None => true,
        Some(t) => t == "dumb",
    }
}

fn var(env: &dyn Environment, name: &str) -> Option<String> {
    env.var(name).map(|v| v.to_string_lossy().into_owned())
}

fn cfg(set: &ConfigSet, name: &str) -> Result<Option<String>, Problem> {
    Ok(config::string(set, name)?.map(|v| v.to_str_lossy().into_owned()))
}

/// The editor command (`git var GIT_EDITOR`); `None` when there is none
/// (dumb terminal and neither `GIT_EDITOR`, `core.editor` nor `EDITOR`).
pub fn editor(set: &ConfigSet, env: &dyn Environment) -> Result<Option<String>, Problem> {
    if let Some(e) = var(env, "GIT_EDITOR") {
        return Ok(Some(e));
    }
    if let Some(e) = cfg(set, "core.editor")? {
        return Ok(Some(e));
    }
    let dumb = terminal_is_dumb(env);
    if !dumb {
        if let Some(e) = var(env, "VISUAL") {
            return Ok(Some(e));
        }
    }
    if let Some(e) = var(env, "EDITOR") {
        return Ok(Some(e));
    }
    Ok((!dumb).then(|| DEFAULT_EDITOR.to_owned()))
}

/// The editor for `rebase -i` todo lists (`git var GIT_SEQUENCE_EDITOR`).
pub fn sequence_editor(set: &ConfigSet, env: &dyn Environment) -> Result<Option<String>, Problem> {
    if let Some(e) = var(env, "GIT_SEQUENCE_EDITOR") {
        return Ok(Some(e));
    }
    if let Some(e) = cfg(set, "sequence.editor")? {
        return Ok(Some(e));
    }
    editor(set, env)
}

/// The pager command (`git var GIT_PAGER`).
pub fn pager(set: &ConfigSet, env: &dyn Environment) -> Result<String, Problem> {
    let p = match var(env, "GIT_PAGER") {
        Some(p) => p,
        None => match config::string_with(set, "core.pager", MissingStyle::Generic)? {
            Some(p) => p.to_str_lossy().into_owned(),
            None => var(env, "PAGER").unwrap_or_else(|| DEFAULT_PAGER.to_owned()),
        },
    };
    Ok(if p.is_empty() { "cat".to_owned() } else { p })
}

/// Bytes that make the shell necessary to run a command string.
fn needs_shell(cmd: &str) -> bool {
    cmd.bytes().any(|b| {
        matches!(
            b,
            b'|' | b'&'
                | b';'
                | b'<'
                | b'>'
                | b'('
                | b')'
                | b'$'
                | b'`'
                | b'\\'
                | b'"'
                | b'\''
                | b' '
                | b'\t'
                | b'\n'
                | b'*'
                | b'?'
                | b'['
                | b'#'
                | b'~'
                | b'='
                | b'%'
                | b'{'
                | b'}'
        )
    })
}

/// A command for `program` (shell syntax allowed) with `args` appended.
pub fn shell_command(program: &str, args: &[&std::ffi::OsStr]) -> Command {
    if needs_shell(program) {
        let mut c = Command::new(SHELL_PATH);
        c.arg("-c").arg(format!("{program} \"$@\"")).arg(program);
        c.args(args);
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    }
}

/// Why the editor did not edit the file. [`EditorError::report`] gives
/// the lines git prints; the caller adds its own advice (for `commit`:
/// "Please supply the message using either -m or -F option.").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorError {
    lines: Vec<(Level, String)>,
}

impl EditorError {
    fn error(message: String) -> Self {
        EditorError {
            lines: vec![(Level::Error, message)],
        }
    }

    /// The lines to print to stderr, each with its level prefix.
    pub fn report(&self) -> &[(Level, String)] {
        &self.lines
    }

    /// Print [`EditorError::report`] to stderr.
    pub fn print(&self) {
        let mut err = std::io::stderr().lock();
        for (level, line) in &self.lines {
            let _ = writeln!(err, "{} {line}", level.prefix());
        }
    }
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.lines.last() {
            Some((_, m)) => f.write_str(m),
            None => Ok(()),
        }
    }
}

impl std::error::Error for EditorError {}

/// How to run the editor.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Show "hint: Waiting for your editor to close the file..." when
    /// stderr is a terminal (advice `waitingForEditor`, on by default;
    /// off with `--no-advice` / `GIT_ADVICE=0`).
    pub hint: bool,
    /// Extra environment variables for the editor.
    pub env: Vec<(OsString, OsString)>,
    /// Directory to run the editor in (default: the current one).
    pub cwd: Option<PathBuf>,
}

/// Whether the `waitingForEditor` advice is enabled by configuration and
/// `GIT_ADVICE` (the caller also honours `--no-advice`).
pub fn waiting_hint_enabled(set: &ConfigSet, env: &dyn Environment) -> bool {
    rgit_messages::Advisor::new(set, env, false)
        .and_then(|a| a.enabled(rgit_messages::Advice::WaitingForEditor))
        .unwrap_or(true)
}

/// Run `editor` (a command string as returned by [`editor`]; `None` is
/// the dumb-terminal error) on `path` and wait for it.
pub fn launch(editor: Option<&str>, path: &Path, opts: &LaunchOptions) -> Result<(), EditorError> {
    launch_with_term(editor, path, opts, &rgit_config::SystemEnv::default())
}

/// [`launch`] with the environment that decides about a dumb terminal.
pub fn launch_with_term(
    editor: Option<&str>,
    path: &Path,
    opts: &LaunchOptions,
    env: &dyn Environment,
) -> Result<(), EditorError> {
    let Some(editor) = editor else {
        return Err(EditorError::error(msg::TERMINAL_IS_DUMB.to_owned()));
    };
    if editor == ":" {
        return Ok(());
    }
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut cmd = shell_command(editor, &[path.as_os_str()]);
    cmd.envs(opts.env.iter().map(|(k, v)| (k, v)));
    if let Some(d) = &opts.cwd {
        cmd.current_dir(d);
    }

    let stderr = std::io::stderr();
    let hint = opts.hint && stderr.is_terminal();
    let erase = hint && !terminal_is_dumb(env);
    if hint {
        let mut e = stderr.lock();
        if erase {
            let _ = write!(e, "hint: {} ", msg::WAITING_FOR_EDITOR);
        } else {
            let _ = writeln!(e, "hint: {}", msg::WAITING_FOR_EDITOR);
        }
        let _ = e.flush();
    }
    let clear_hint = || {
        if erase {
            let mut e = std::io::stderr().lock();
            let _ = write!(e, "\r\x1b[K");
            let _ = e.flush();
        }
    };

    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            clear_hint();
            let reason = strerror(&e);
            let first = if editor.contains('/') && !needs_shell(editor) {
                (Level::Fatal, msg::cannot_exec(editor, &reason))
            } else {
                (Level::Error, msg::cannot_run(editor, &reason))
            };
            return Err(EditorError {
                lines: vec![first, (Level::Error, msg::unable_to_start_editor(editor))],
            });
        }
    };
    clear_hint();
    if status.success() {
        return Ok(());
    }
    let mut lines = Vec::new();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            lines.push((Level::Error, msg::died_of_signal(editor, sig)));
        }
    }
    lines.push((Level::Error, msg::editor_problem(editor)));
    Err(EditorError { lines })
}

/// `strerror`-style text of an I/O error ("No such file or directory").
fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::MapEnv;

    fn set(entries: &str) -> ConfigSet {
        rgit_config::parse_bytes(
            entries.as_bytes(),
            rgit_config::Origin::CommandLine,
            rgit_config::Scope::Command,
        )
        .unwrap()
    }

    #[test]
    fn editor_order() {
        let none = set("");
        let env = MapEnv::new([("TERM", "dumb")]);
        assert_eq!(editor(&none, &env).unwrap(), None);
        let env = MapEnv::new([("TERM", "dumb"), ("VISUAL", "v"), ("EDITOR", "e")]);
        assert_eq!(editor(&none, &env).unwrap().as_deref(), Some("e"));
        let env = MapEnv::new([("TERM", "xterm"), ("VISUAL", "v"), ("EDITOR", "e")]);
        assert_eq!(editor(&none, &env).unwrap().as_deref(), Some("v"));
        let env = MapEnv::new([("TERM", "xterm")]);
        assert_eq!(editor(&none, &env).unwrap().as_deref(), Some("vi"));
        let core = set("[core]\neditor = c\n");
        let env = MapEnv::new([("EDITOR", "e")]);
        assert_eq!(editor(&core, &env).unwrap().as_deref(), Some("c"));
        let env = MapEnv::new([("GIT_EDITOR", "")]);
        assert_eq!(editor(&core, &env).unwrap().as_deref(), Some(""));
        let seq = set("[sequence]\neditor = s\n");
        let env = MapEnv::new([("GIT_EDITOR", "g")]);
        assert_eq!(sequence_editor(&seq, &env).unwrap().as_deref(), Some("s"));
        assert_eq!(sequence_editor(&none, &env).unwrap().as_deref(), Some("g"));
    }

    #[test]
    fn pager_order() {
        let none = set("");
        assert_eq!(pager(&none, &MapEnv::default()).unwrap(), "less");
        assert_eq!(pager(&none, &MapEnv::new([("PAGER", "")])).unwrap(), "cat");
        let core = set("[core]\npager = most\n");
        assert_eq!(
            pager(&core, &MapEnv::new([("PAGER", "more")])).unwrap(),
            "most"
        );
        assert_eq!(
            pager(&core, &MapEnv::new([("GIT_PAGER", "")])).unwrap(),
            "cat"
        );
    }

    #[test]
    fn shell_detection() {
        assert!(!needs_shell("vi"));
        assert!(!needs_shell("/usr/bin/vim"));
        assert!(needs_shell("./e\\ space.sh"));
        assert!(needs_shell("code --wait"));
        assert!(needs_shell("$EDITOR"));
    }
}
