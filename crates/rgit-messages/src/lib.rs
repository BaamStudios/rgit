// SPDX-License-Identifier: MIT
//! Conventions of the messages rgit prints, shared by all commands.
//!
//! Behaviour as documented (`Documentation/technical/api-error-handling.adoc`,
//! `Documentation/config/advice.adoc`, `Documentation/git.adoc`) and observed
//! from the reference git 2.56:
//!
//! - A message to the user starts with a prefix that names its severity:
//!   `fatal: ` (the command stops, exit code [`EXIT_FATAL`]), `error: `
//!   (the command fails, usually exit code [`EXIT_ERROR`]), `warning: `,
//!   `usage: ` (wrong invocation, exit code [`EXIT_USAGE`]). Only the first
//!   line of a multi-line `fatal`/`error`/`warning` message carries the
//!   prefix ([`prefixed`]).
//! - A hint prefixes every line with `hint: `; an empty line becomes
//!   `hint:` without the trailing space, and a final newline of the text
//!   adds no extra line ([`hint_lines`]).
//! - A system error is appended as `: <strerror>` ([`with_strerror`]).
//! - Advice ([`advice`]) is a hint that the user can switch off with
//!   `advice.<name>=false`, `GIT_ADVICE=0` or `git --no-advice`.
//!
//! User-visible strings are reproduced verbatim (CLEANROOM.md, "Decisions on
//! borderline material") and collected in [`advice`] for the advice texts
//! shared between crates.

pub mod advice;
mod messages;

pub use advice::{Advice, Advisor};

/// Exit code of `fatal:` (git's `die`).
pub const EXIT_FATAL: i32 = 128;
/// Exit code of a usage error (`usage:` or an unknown option).
pub const EXIT_USAGE: i32 = 129;
/// Exit code of a command that failed with `error:`.
pub const EXIT_ERROR: i32 = 1;

/// The severity of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `fatal: `
    Fatal,
    /// `error: `
    Error,
    /// `warning: `
    Warning,
    /// `hint: ` (on every line, see [`hint_lines`]).
    Hint,
    /// `usage: `
    Usage,
}

impl Level {
    /// The prefix including the trailing space.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Level::Fatal => "fatal: ",
            Level::Error => "error: ",
            Level::Warning => "warning: ",
            Level::Hint => "hint: ",
            Level::Usage => "usage: ",
        }
    }
}

/// `<prefix><msg>`: the prefix goes before the first line only. For
/// [`Level::Hint`] every line is prefixed (as [`hint_lines`] does).
#[must_use]
pub fn prefixed(level: Level, msg: &str) -> String {
    if level == Level::Hint {
        return hint_lines(msg).join("\n");
    }
    format!("{}{msg}", level.prefix())
}

/// The lines of a hint: `hint: <line>` for each line of `text`, `hint:`
/// for an empty line. A trailing newline does not start another line.
#[must_use]
pub fn hint_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                "hint:".to_owned()
            } else {
                format!("hint: {l}")
            }
        })
        .collect()
}

/// A hint as written to stderr: every line from [`hint_lines`], wrapped
/// in `color` and the reset sequence when given, each ending in `\n`.
#[must_use]
pub fn render_hint(text: &str, color: Option<&str>) -> String {
    let mut out = String::new();
    for line in hint_lines(text) {
        match color {
            Some(c) => {
                out.push_str(c);
                out.push_str(&line);
                out.push_str(RESET);
            }
            None => out.push_str(&line),
        }
        out.push('\n');
    }
    out
}

/// The SGR reset sequence that ends a colored line.
pub const RESET: &str = "\x1b[m";

/// `strerror`-style text of an I/O error (Rust's " (os error N)" removed),
/// e.g. `No such file or directory`.
#[must_use]
pub fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// `<msg>: <strerror>`, the form of messages about a failed system call.
#[must_use]
pub fn with_strerror(msg: &str, e: &std::io::Error) -> String {
    format!("{msg}: {}", strerror(e))
}

pub use messages::bad_bool_env;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(prefixed(Level::Fatal, "x"), "fatal: x");
        assert_eq!(prefixed(Level::Error, "a\nb"), "error: a\nb");
        assert_eq!(prefixed(Level::Warning, "w"), "warning: w");
        assert_eq!(prefixed(Level::Usage, "u"), "usage: u");
        assert_eq!(
            prefixed(Level::Hint, "a\n\n\tb"),
            "hint: a\nhint:\nhint: \tb"
        );
    }

    #[test]
    fn hint_rendering() {
        assert_eq!(render_hint("a\n", None), "hint: a\n");
        assert_eq!(render_hint("a\n\n", None), "hint: a\nhint:\n");
        assert_eq!(
            render_hint("a\n\nb", Some("\x1b[33m")),
            "\x1b[33mhint: a\x1b[m\n\x1b[33mhint:\x1b[m\n\x1b[33mhint: b\x1b[m\n"
        );
    }

    #[test]
    fn system_errors() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(with_strerror("x", &e), "x: No such file or directory");
    }
}
