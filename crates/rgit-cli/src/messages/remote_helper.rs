// SPDX-License-Identifier: MIT
//! Messages of the built-in remote helpers `git remote-ext` and
//! `git remote-fd`, as printed by the reference git.

pub const EXT_USAGE: &str = "usage: git remote-ext <remote> <url>";
pub const FD_USAGE: &str = "usage: git remote-fd <remote> <url>";

/// Printed (without newline) for a command the helper does not know.
pub const BAD_COMMAND: &str = "Bad command";

/// `fatal:` for an fd:: address that is not `<in>[,<out>][/<anything>]`.
pub const BAD_URL_SYNTAX: &str = "Bad URL syntax";

/// `fatal:` for an unknown `%` sequence of an ext:: command.
pub fn bad_placeholder(c: char) -> String {
    format!("Bad remote-ext placeholder '%{c}'.")
}

/// `fatal:` for a `%` at the end of an ext:: command.
pub const INCOMPLETE_PLACEHOLDER: &str = "remote-ext command has incomplete placeholder";

/// `fatal:` when the ext:: command is empty.
pub const EMPTY_COMMAND: &str = "remote-ext command is empty";

/// `fatal:` when copying between the file descriptors fails.
pub const COPY_FAILED: &str = "Copying data between file descriptors failed";
