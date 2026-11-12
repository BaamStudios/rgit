// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git status` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git status [<options>] [--] [<pathspec>...]"];

pub const HELP_BRANCH: &str = "show branch information";
pub const HELP_PORCELAIN: &str = "machine-readable output";
pub const HELP_Z: &str = "terminate entries with NUL";
pub const HELP_UNTRACKED: &str =
    "show untracked files, optional modes: all, normal, no. (Default: all)";

pub fn invalid_untracked(mode: &str) -> String {
    format!("Invalid untracked files mode '{mode}'")
}

pub fn unsupported_porcelain(v: &str) -> String {
    format!("unsupported porcelain version '{v}'")
}

pub fn not_supported(what: &str) -> String {
    format!("rgit: status: {what} is not supported yet")
}
