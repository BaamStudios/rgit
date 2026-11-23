// SPDX-License-Identifier: MIT
//! Messages and usage of `git shortlog` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git shortlog [<options>] [<revision-range>] [[--] <path>...]",
    "git log --pretty=short | git shortlog [<options>]",
];
pub const HELP_COMMITTER: &str = "group by committer rather than author";
pub const HELP_NUMBERED: &str = "sort output according to the number of commits per author";
pub const HELP_SUMMARY: &str = "suppress commit descriptions, only provides commit count";
pub const HELP_EMAIL: &str = "show the email address of each author";
pub const ARGH_WRAP: &str = "<w>[,<i1>[,<i2>]]";
pub const HELP_WRAP: &str = "linewrap output";
pub const HELP_GROUP: &str = "group by field";
pub const BAD_WRAP: &str = "-w[<width>[,<indent1>[,<indent2>]]]";
pub const TOO_MANY_ARGUMENTS: &str = "too many arguments given outside repository";
pub const STDIN_MULTIPLE: &str = "using multiple --group options with stdin is not supported";
pub const STDIN_TRAILER: &str = "using --group=trailer with stdin is not supported";
pub const STDIN_FORMAT: &str = "using --group=format with stdin is not supported";
/// The description of a commit without a subject.
pub const NONE: &str = "<none>";

pub fn unknown_group(kind: &str) -> String {
    format!("unknown group type: {kind}")
}

/// How git names an option it does not know.
pub fn unknown_option(arg: &str) -> String {
    format!("unknown option `{arg}'")
}
