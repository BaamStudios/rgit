// SPDX-License-Identifier: MIT
//! `git shell` (Documentation/git-shell.adoc): a login shell that only
//! runs the server side of git.
//!
//! `git shell -c <command>` (what sshd calls for `ssh host <command>`)
//! runs `git-upload-pack`, `git-receive-pack` or `git-upload-archive`
//! with exactly one argument, quoted the way git's clients quote the
//! repository path (`'path'`, with `'\''` and `'\!'` for the two special
//! characters), or a program in `~/git-shell-commands` with arguments
//! split like a shell command line. Without arguments it is an
//! interactive shell (`git> `) for the programs in that directory, if the
//! directory exists; a `no-interactive-login` program there is run
//! instead of the shell, a `help` program before the first prompt.
//!
//! The details the documentation leaves open follow git 2.56 (observed):
//!
//! - `git <service>` is accepted for `git-<service>`; the argument must
//!   follow after one space and must not start with `-`.
//! - A command name must not contain `.` or `/`.
//! - Words are split at white space, also at the start and the end of the
//!   line (an empty word each); `'...'` is literal, `"..."` and the rest
//!   know `\` as escape character.
//! - Interactive: the prompt and the messages go to standard error; an
//!   empty first word is ignored; `quit`, `exit`, `logout` and `bye` end
//!   the shell; a line of 4 MiB minus one byte or more (with its end of
//!   line) is fatal.

use crate::prelude::*;

#[cfg(not(unix))]
pub fn run(_ctx: &mut Context, _argv: &[String]) -> CmdResult {
    Err(Die::Fatal(
        "rgit shell is not supported on this platform".to_owned(),
    ))
}

#[cfg(unix)]
pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    match &argv[1..] {
        [] => imp::interactive(),
        [option, command] if option == "-c" => imp::command(command),
        _ => Err(Die::Fatal(crate::messages::shell::USAGE.to_owned())),
    }
}

/// Undo the quoting of a repository path: `'...'` with `'\''` and `'\!'`
/// in between; `None` for anything else.
fn sq_dequote(arg: &str) -> Option<String> {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg.strip_prefix('\'')?;
    loop {
        let (inside, after) = rest.split_once('\'')?;
        out.push_str(inside);
        if after.is_empty() {
            return Some(out);
        }
        // `\'` or `\!`, then the next quoted part.
        let escaped = after.strip_prefix('\\')?;
        let c = escaped.chars().next().filter(|c| matches!(c, '\'' | '!'))?;
        out.push(c);
        rest = escaped[1..].strip_prefix('\'')?;
    }
}

/// Split a command line into words (see the module documentation). The
/// error is git's reason.
fn split_cmdline(line: &str) -> Result<Vec<String>, &'static str> {
    use crate::messages::shell as msg;
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            None if c.is_ascii_whitespace() => {
                words.push(std::mem::take(&mut word));
                while chars.next_if(|n| n.is_ascii_whitespace()).is_some() {}
            }
            None if c == '\'' || c == '"' => quote = Some(c),
            Some(q) if c == q => quote = None,
            Some('\'') => word.push(c),
            _ if c == '\\' => match chars.next() {
                Some(next) => word.push(next),
                None => return Err(msg::ENDS_WITH_BACKSLASH),
            },
            _ => word.push(c),
        }
    }
    if quote.is_some() {
        return Err(msg::UNCLOSED_QUOTE);
    }
    words.push(word);
    Ok(words)
}

/// Whether `name` may name a program in `git-shell-commands`.
fn is_valid_command_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['.', '/'])
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::io::{BufRead, Read, Write};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Command;

    use super::{is_valid_command_name, split_cmdline, sq_dequote};
    use crate::messages::shell as msg;
    use crate::prelude::*;

    /// Where the user's own commands live, below the home directory.
    const COMMAND_DIR: &str = "git-shell-commands";
    /// Run before the first prompt, if it exists.
    const HELP_COMMAND: &str = "git-shell-commands/help";
    /// Run instead of the interactive shell, if it exists.
    const NO_INTERACTIVE_LOGIN: &str = "git-shell-commands/no-interactive-login";
    /// The longest line the interactive shell reads, with its end of
    /// line.
    const MAX_LINE: usize = 4 * 1024 * 1024 - 1;

    /// The server programs a client may ask for.
    const SERVICES: [&str; 3] = ["git-receive-pack", "git-upload-pack", "git-upload-archive"];

    fn access(path: &str, mode: libc::c_int) -> bool {
        let Ok(path) = CString::new(path) else {
            return false;
        };
        // SAFETY: access(2) with a valid C string.
        unsafe { libc::access(path.as_ptr(), mode) == 0 }
    }

    fn cd_to_homedir() -> Result<(), Die> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .ok_or_else(|| Die::Fatal(msg::HOME_UNSET.to_owned()))?;
        std::env::set_current_dir(home).map_err(|_| Die::Fatal(msg::CANNOT_CHDIR_HOME.to_owned()))
    }

    /// Replace this process with `command`; only returns on failure.
    fn exec(mut command: Command, name: &str) -> Die {
        let error = command.exec();
        Die::Fatal(msg::cannot_exec(name, &rgit_messages::strerror(&error)))
    }

    /// `git shell -c <command>`.
    pub fn command(line: &str) -> CmdResult {
        // `git upload-pack` for `git-upload-pack`.
        let dashed = match line.strip_prefix("git ") {
            Some(rest) => format!("git-{rest}"),
            None => line.to_owned(),
        };
        for service in SERVICES {
            let arg = match dashed.strip_prefix(service) {
                Some("") => None,
                Some(rest) => match rest.strip_prefix(' ') {
                    Some(arg) => Some(arg),
                    // A longer name.
                    None => continue,
                },
                None => continue,
            };
            let Some(arg) = arg.and_then(sq_dequote).filter(|a| !a.starts_with('-')) else {
                return Err(Die::Fatal(msg::BAD_ARGUMENT.to_owned()));
            };
            let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
            let mut command = Command::new(exe);
            command.arg(&service["git-".len()..]).arg(arg);
            return Err(exec(command, service));
        }

        cd_to_homedir()?;
        let words =
            split_cmdline(line).map_err(|e| Die::Fatal(msg::invalid_command_format(line, e)))?;
        let name = words[0].as_str();
        let path = format!("{COMMAND_DIR}/{name}");
        if is_valid_command_name(name) && access(&path, libc::X_OK) {
            let mut command = Command::new(&path);
            command.args(&words[1..]);
            // What cannot be run (a directory) is no command either.
            let _ = command.exec();
        }
        Err(Die::Fatal(msg::unrecognized_command(line)))
    }

    /// Run a program of `git-shell-commands` and wait for it.
    fn run_command(path: &str, args: &[String]) {
        match Command::new(path).args(args).status() {
            Ok(status) => {
                if let Some(signal) = status.signal() {
                    eprintln!("error: {}", msg::died_of_signal(path, signal));
                }
            }
            Err(e) => eprintln!(
                "fatal: {}",
                msg::cannot_exec(path, &rgit_messages::strerror(&e))
            ),
        }
    }

    /// `git shell` without arguments.
    pub fn interactive() -> CmdResult {
        cd_to_homedir()?;
        if !access(COMMAND_DIR, libc::R_OK | libc::X_OK) {
            eprintln!("fatal: {}", msg::NOT_ENABLED);
            eprintln!("hint: {}", msg::NOT_ENABLED_HINT);
            return Err(Die::Exit(rgit_messages::EXIT_FATAL));
        }
        if access(NO_INTERACTIVE_LOGIN, libc::X_OK) {
            let status = Command::new(NO_INTERACTIVE_LOGIN).status().map_err(|e| {
                Die::Fatal(msg::cannot_exec(
                    NO_INTERACTIVE_LOGIN,
                    &rgit_messages::strerror(&e),
                ))
            })?;
            return Ok(status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)));
        }
        if access(HELP_COMMAND, libc::X_OK) {
            run_command(HELP_COMMAND, &[]);
        }

        let mut stdin = std::io::stdin().lock();
        let mut stderr = std::io::stderr();
        loop {
            let _ = stderr.write_all(msg::PROMPT.as_bytes());
            let _ = stderr.flush();
            let mut raw = Vec::new();
            let read = (&mut stdin)
                .take(MAX_LINE as u64)
                .read_until(b'\n', &mut raw)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if read == 0 {
                let _ = stderr.write_all(b"\n");
                return Ok(0);
            }
            if raw.len() >= MAX_LINE {
                return Err(Die::Fatal(msg::INPUT_TOO_LONG.to_owned()));
            }
            if raw.ends_with(b"\n") {
                raw.pop();
                if raw.ends_with(b"\r") {
                    raw.pop();
                }
            }
            let line = String::from_utf8_lossy(&raw).into_owned();
            let words = match split_cmdline(&line) {
                Ok(words) => words,
                Err(e) => {
                    eprintln!("{}", msg::invalid_command_format(&line, e));
                    continue;
                }
            };
            let name = words[0].as_str();
            if name.is_empty() {
                continue;
            }
            if matches!(name, "quit" | "logout" | "exit" | "bye") {
                return Ok(0);
            }
            if !is_valid_command_name(name) {
                eprintln!("{}", msg::invalid_command_name(name));
                continue;
            }
            let path = format!("{COMMAND_DIR}/{name}");
            // What is there is tried, runnable or not.
            if access(&path, libc::F_OK) {
                run_command(&path, &words[1..]);
            } else {
                eprintln!("{}", msg::unrecognized_command(name));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_valid_command_name, split_cmdline, sq_dequote};

    #[test]
    fn dequote() {
        assert_eq!(sq_dequote("'r.git'").as_deref(), Some("r.git"));
        assert_eq!(sq_dequote("''").as_deref(), Some(""));
        assert_eq!(sq_dequote("'r'\\''x.git'").as_deref(), Some("r'x.git"));
        assert_eq!(sq_dequote("'a'\\!'b'").as_deref(), Some("a!b"));
        for bad in [
            "r.git",
            "'r.git",
            "'r.git' ",
            " 'r.git'",
            "'r.git' 'b'",
            "'a'\\\\'b'",
            "'a'\\'",
            "",
        ] {
            assert_eq!(sq_dequote(bad), None, "{bad:?}");
        }
    }

    // Words as git 2.56 hands them to a program in git-shell-commands.
    #[test]
    fn split() {
        let split = |line: &str| split_cmdline(line).unwrap();
        assert_eq!(split("ping"), ["ping"]);
        assert_eq!(split("ping a 'b c'"), ["ping", "a", "b c"]);
        assert_eq!(split("args a\\ b \"c\\\"d\" 'e\\f'"), ["args", "a b", "c\"d", "e\\f"]);
        assert_eq!(split("args a\tb   c"), ["args", "a", "b", "c"]);
        assert_eq!(split("args \"\" x"), ["args", "", "x"]);
        // White space at either end makes an empty word.
        assert_eq!(split("ping "), ["ping", ""]);
        assert_eq!(split("  ping  "), ["", "ping", ""]);
        assert_eq!(split(""), [""]);
        assert_eq!(split_cmdline("ping 'x"), Err("unclosed quote"));
        assert_eq!(split_cmdline("args \"a"), Err("unclosed quote"));
        assert_eq!(split_cmdline("args a\\"), Err("cmdline ends with \\"));
    }

    #[test]
    fn command_names() {
        for good in ["ping", "a-b", "a_b", "sp ace", "-x"] {
            assert!(is_valid_command_name(good), "{good:?}");
        }
        for bad in ["", "p.sh", ".hidden", "sub/x", "..", "/bin/ls"] {
            assert!(!is_valid_command_name(bad), "{bad:?}");
        }
    }
}
