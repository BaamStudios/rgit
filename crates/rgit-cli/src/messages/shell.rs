// SPDX-License-Identifier: MIT
//! Messages of `git shell`, word for word as git 2.56 prints them
//! (observed from the binary).

// `git shell` exists on Unix only so far (#682).
#![cfg_attr(not(unix), allow(dead_code))]

/// Anything but no arguments or `-c <command>`.
pub const USAGE: &str = "Run with no arguments or with -c cmd";

/// Interactive use without a `~/git-shell-commands` directory.
pub const NOT_ENABLED: &str = "Interactive git shell is not enabled.";
pub const NOT_ENABLED_HINT: &str =
    "~/git-shell-commands should exist and have read and execute access.";

/// A service command without exactly one quoted argument.
pub const BAD_ARGUMENT: &str = "bad argument";

/// Neither a service command nor one in `git-shell-commands`.
pub fn unrecognized_command(command: &str) -> String {
    format!("unrecognized command '{command}'")
}

/// A command line that cannot be split into words.
pub fn invalid_command_format(command: &str, reason: &str) -> String {
    format!("invalid command format '{command}': {reason}")
}

/// Interactive: a command name with `.` or `/`.
pub fn invalid_command_name(name: &str) -> String {
    format!("invalid command format '{name}'")
}

/// Interactive: a line longer than the shell reads.
pub const INPUT_TOO_LONG: &str = "invalid command format: input too long";

/// Why a command line cannot be split.
pub const UNCLOSED_QUOTE: &str = "unclosed quote";
pub const ENDS_WITH_BACKSLASH: &str = "cmdline ends with \\";

pub const HOME_UNSET: &str = "could not determine user's home directory; HOME is unset";
pub const CANNOT_CHDIR_HOME: &str = "could not chdir to user's home directory";

/// A command could not be started.
pub fn cannot_exec(path: &str, reason: &str) -> String {
    format!("cannot exec '{path}': {reason}")
}

/// Interactive: a command ended by a signal (printed as `error: ...`).
pub fn died_of_signal(path: &str, signal: i32) -> String {
    format!("{path} died of signal {signal}")
}

/// The interactive prompt.
pub const PROMPT: &str = "git> ";
